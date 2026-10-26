//! Coordinator eligibility, dispatch and outcomes (spec 4.7 and 8).
//!
//! Which of a provider's sources need a call, how the calls are sent under
//! the transport limits, and what stored changes a call's outcome becomes.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{StatusCode, header};
use web_time::Instant;

use super::catalog::ProviderCatalog;
use super::descriptor::{Input, ProviderDescriptor, ResolveKind, Resolves};
use super::exchange::{ProviderRequest, ProviderResponse};
use super::ids::{IdentityProviderId, SourceDomain, UnixTime};
use super::input::{RandomSource, ResolveInput, ResolveSignals};
use super::mutation::{PartnerIdChange, ProviderResult, ProviderStateChange, StateUpdate};
use super::policy::{PolicySignals, policy_allows};
use super::provider::IdentityProvider;
use super::resolution::{
    EntrySet, EntrySets, IdentityProviderError, Resolution, ResolvedEid, StateChange, entry_sets,
};
use super::revision::{IdentityKeys, StateRevision, record_revision, uid_hash, visitor_key};
use crate::consent::ConsentContext;
use crate::ec::kv_types::{KvEntry, KvOptOut, KvPartnerId};
use crate::ec::rate_limiter::RateLimiter;
use crate::platform::{
    PlatformBackendSpec, PlatformHttpRequest, PlatformPendingRequest, PlatformResponse,
    RuntimeServices,
};
use crate::settings::{IdentitySettings, ProviderMode, ProviderSettings, Settings};
use crate::streaming_processor::{BodyStreamDecoder, Compression};

/// How long a burst-capped provider waits: one limiter window.
const BURST_WAIT_SECS: u64 = 60;
/// The wait after a `no_match` that gives no `retry_in`.
const NO_MATCH_WAIT_SECS: u64 = 86_400;
/// The longest `retry_in` the coordinator honours.
const MAX_RETRY_SECS: u64 = 7 * 86_400;
/// First wait after an input-caused failure.
const INPUT_BACKOFF_BASE_SECS: u64 = 3_600;
/// Longest wait after an input-caused failure.
const INPUT_BACKOFF_CAP_SECS: u64 = 86_400;
/// First wait after a transport failure.
const TRANSPORT_BACKOFF_BASE_SECS: u64 = 300;
/// Longest wait after a transport failure.
const TRANSPORT_BACKOFF_CAP_SECS: u64 = 3_600;

/// One provider's `ps` entry, as the request read it.
// Temporary: the request wiring, the first non-test caller, arrives in a later task.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
struct ProviderView {
    n: Option<u64>,
    f: u32,
    o: Option<KvOptOut>,
    oc: Option<u64>,
    iv: Vec<String>,
    ov: Vec<String>,
    st: Option<serde_json::Value>,
}

/// What eligibility and outcomes read from the KV row the request read
/// (spec 4.7), and nothing else. It never reads `ts-eids-s`: every removal is
/// already in KV (spec 5.3).
// Temporary: the request wiring, the first non-test caller, arrives in a later task.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub(crate) struct EligibilityView {
    records: BTreeMap<SourceDomain, KvPartnerId>,
    providers: BTreeMap<IdentityProviderId, ProviderView>,
}

impl EligibilityView {
    /// The view of the row the request read. A key that is not a valid
    /// source domain or provider ID cannot name a configured one and is
    /// skipped.
    #[must_use]
    // Temporary: the request wiring, the first non-test caller, arrives in a later task.
    #[allow(dead_code)]
    pub(crate) fn from_kv(entry: &KvEntry) -> Self {
        let records = entry
            .ids
            .iter()
            .filter_map(|(source, record)| {
                Some((source.parse::<SourceDomain>().ok()?, record.clone()))
            })
            .collect();
        let providers = entry
            .ps
            .iter()
            .filter_map(|(provider, state)| {
                let view = ProviderView {
                    n: state.n,
                    f: state.f,
                    o: state.o.clone(),
                    oc: state.oc,
                    iv: state.iv.clone(),
                    ov: state.ov.clone(),
                    st: state.st.clone(),
                };
                Some((provider.parse::<IdentityProviderId>().ok()?, view))
            })
            .collect();
        Self { records, providers }
    }

    /// A view of a row with no records and no provider state.
    #[must_use]
    // Temporary: the request wiring, the first non-test caller, arrives in a later task.
    #[allow(dead_code)]
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// The whole record of `source`, if the row holds one.
    #[must_use]
    pub(crate) fn record(&self, source: &SourceDomain) -> Option<&KvPartnerId> {
        self.records.get(source)
    }

    /// Every stored record.
    pub(crate) fn records(&self) -> impl Iterator<Item = (&SourceDomain, &KvPartnerId)> {
        self.records.iter()
    }

    /// The provider's consecutive failures, `f`.
    #[must_use]
    pub(crate) fn failures(&self, provider: &IdentityProviderId) -> u32 {
        self.providers.get(provider).map_or(0, |view| view.f)
    }

    /// The provider's `n`: the earliest time it may be called.
    #[must_use]
    pub(crate) fn next_eligible(&self, provider: &IdentityProviderId) -> Option<u64> {
        self.providers.get(provider).and_then(|view| view.n)
    }

    /// The provider's private state `st`, which a call reads.
    #[must_use]
    // Temporary: the request wiring, the first non-test caller, arrives in a later task.
    #[allow(dead_code)]
    pub(crate) fn state(&self, provider: &IdentityProviderId) -> Option<&serde_json::Value> {
        self.providers
            .get(provider)
            .and_then(|view| view.st.as_ref())
    }

    /// Whether the provider's opt-out removed a UID with this hash, `ps.ov`.
    #[must_use]
    // Temporary: the request wiring, the first non-test caller, arrives in a later task.
    #[allow(dead_code)]
    pub(crate) fn has_opt_out_hash(&self, provider: &IdentityProviderId, hash: &str) -> bool {
        self.providers
            .get(provider)
            .is_some_and(|view| view.ov.iter().any(|known| known == hash))
    }

    /// Whether the provider invalidated a UID with this hash, `ps.iv`.
    fn has_invalidated_hash(&self, provider: &IdentityProviderId, hash: &str) -> bool {
        self.providers
            .get(provider)
            .is_some_and(|view| view.iv.iter().any(|known| known == hash))
    }

    /// Whether `o` is set and `oc` is not strictly later than `o.at`.
    #[must_use]
    pub(crate) fn is_opted_out(&self, provider: &IdentityProviderId) -> bool {
        self.providers.get(provider).is_some_and(|view| {
            view.o
                .as_ref()
                .is_some_and(|opt_out| view.oc.is_none_or(|clear| clear <= opt_out.at))
        })
    }

    #[cfg(test)]
    fn provider_mut(&mut self, provider: &str) -> &mut ProviderView {
        self.providers
            .entry(provider.parse().expect("should parse the provider ID"))
            .or_default()
    }

    #[cfg(test)]
    pub(crate) fn set_record_for_test(&mut self, source: &SourceDomain, record: KvPartnerId) {
        self.records.insert(source.clone(), record);
    }

    #[cfg(test)]
    pub(crate) fn set_failures_for_test(&mut self, provider: &str, failures: u32) {
        self.provider_mut(provider).f = failures;
    }

    #[cfg(test)]
    pub(crate) fn set_next_for_test(&mut self, provider: &str, next: u64) {
        self.provider_mut(provider).n = Some(next);
    }

    #[cfg(test)]
    pub(crate) fn set_invalidated_for_test(&mut self, provider: &str, hashes: Vec<String>) {
        self.provider_mut(provider).iv = hashes;
    }

    #[cfg(test)]
    pub(crate) fn set_opt_out_for_test(&mut self, provider: &str, at: u64, clear: Option<u64>) {
        let view = self.provider_mut(provider);
        view.o = Some(KvOptOut {
            at,
            fp: "0".to_owned(),
        });
        view.oc = clear;
    }
}

/// What the request is, as eligibility reads it.
// Temporary: the request wiring, the first non-test caller, arrives in a later task.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct RequestFacts {
    /// The client looks like a real browser (step 1).
    pub(crate) is_real_browser: bool,
    /// Global consent allows EC and EID persistence (step 2).
    pub(crate) ec_persistence_allowed: bool,
    /// The signals the consent context does not carry (step 3).
    pub(crate) policy_signals: PolicySignals,
    /// This is the writer request, not a stager (step 7).
    pub(crate) writer: bool,
    /// This is a new visitor's first navigation (step 7).
    pub(crate) new_visitor: bool,
    /// The navigation runs an auction (step 7).
    pub(crate) runs_auction: bool,
    /// The request's time.
    pub(crate) now: UnixTime,
    /// The inputs this request can supply (step 4).
    pub(crate) available_inputs: BTreeSet<Input>,
}

/// When a provider is dispatched, relative to the navigation response.
// Temporary: the request wiring, the first non-test caller, arrives in a later task.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Before the auction, within the hold budget.
    Hold,
    /// After the response is sent.
    AfterSend,
}

/// Why a provider was not eligible. Each variant names the step of spec 4.7
/// that stopped it. The breaker (step 6) and the burst cap (step 8) are
/// checked elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkipReason {
    /// The provider declares no `resolves`, so it never makes a call.
    NotAResolver,
    /// Step 1: the client does not look like a real browser.
    NotRealBrowser,
    /// Step 2: global consent does not allow EID persistence.
    EcPersistenceNotAllowed,
    /// Step 3: the provider's consent policy is not met.
    ConsentPolicy,
    /// Step 4: no source needs a call this time.
    NoSourceDue,
    /// Step 5: `ps.n` has not passed.
    NotYetEligible,
    /// Step 5: the visitor opted out of the provider.
    OptedOut,
    /// Step 7: this is a new visitor's first navigation.
    NewVisitor,
    /// Step 7: this request is a stager.
    NotWriter,
    /// Step 7: the provider's mode runs it in the other phase.
    WrongPhase,
}

/// Whether a provider is eligible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Eligibility {
    /// It is, for these sources.
    Eligible {
        kinds: BTreeMap<SourceDomain, ResolveKind>,
    },
    /// It is not.
    Skip(SkipReason),
}

/// What [`eligibility`] reads about one provider.
// Temporary: the request wiring, the first non-test caller, arrives in a later task.
#[allow(dead_code)]
pub(crate) struct EligibilityInput<'a> {
    pub(crate) provider: &'a dyn IdentityProvider,
    pub(crate) settings: &'a ProviderSettings,
    pub(crate) identity: &'a IdentitySettings,
    pub(crate) catalog: &'a ProviderCatalog,
    pub(crate) consent: &'a ConsentContext,
}

/// Whether the provider is eligible for this request, steps 1 to 7 of spec
/// 4.7 in order, and which sources need which kind of call.
// Temporary: the request wiring, the first non-test caller, arrives in a later task.
#[allow(dead_code)]
#[must_use]
pub(crate) fn eligibility(
    input: &EligibilityInput<'_>,
    view: &EligibilityView,
    facts: &RequestFacts,
    phase: Phase,
) -> Eligibility {
    let descriptor = input.provider.descriptor();
    let id = descriptor.id();
    if descriptor.resolves().is_none() {
        return Eligibility::Skip(SkipReason::NotAResolver);
    }
    if !facts.is_real_browser {
        return Eligibility::Skip(SkipReason::NotRealBrowser);
    }
    if !facts.ec_persistence_allowed {
        return Eligibility::Skip(SkipReason::EcPersistenceNotAllowed);
    }
    if !policy_allows(
        descriptor.consent_policy(),
        input.consent,
        facts.policy_signals,
    ) {
        return Eligibility::Skip(SkipReason::ConsentPolicy);
    }
    let kinds = source_kinds(input, view, facts);
    if kinds.is_empty() {
        log::debug!("Skipping identity provider '{id}': no source needs a call");
        return Eligibility::Skip(SkipReason::NoSourceDue);
    }
    if view
        .next_eligible(id)
        .is_some_and(|next| facts.now.as_secs() < next)
    {
        return Eligibility::Skip(SkipReason::NotYetEligible);
    }
    if view.is_opted_out(id) {
        return Eligibility::Skip(SkipReason::OptedOut);
    }
    if facts.new_visitor {
        return Eligibility::Skip(SkipReason::NewVisitor);
    }
    if !facts.writer {
        return Eligibility::Skip(SkipReason::NotWriter);
    }
    let runs_in_phase = match (phase, input.settings.mode) {
        (Phase::Hold, ProviderMode::Hold) => facts.runs_auction,
        (Phase::AfterSend, ProviderMode::AfterSend) => true,
        (Phase::AfterSend, ProviderMode::Hold) => !facts.runs_auction,
        (Phase::Hold, ProviderMode::AfterSend) => false,
    };
    if !runs_in_phase {
        return Eligibility::Skip(SkipReason::WrongPhase);
    }
    Eligibility::Eligible { kinds }
}

/// Step 4: the kind each claimed source needs, leaving out a source whose
/// kind the provider does not list or whose needs the request cannot meet.
fn source_kinds(
    input: &EligibilityInput<'_>,
    view: &EligibilityView,
    facts: &RequestFacts,
) -> BTreeMap<SourceDomain, ResolveKind> {
    let descriptor = input.provider.descriptor();
    let Some(resolves) = descriptor.resolves() else {
        return BTreeMap::new();
    };
    input
        .catalog
        .sources_of(descriptor.id())
        .iter()
        .filter_map(|source| {
            let kind = source_kind(input, view, facts, source)?;
            let listed = resolves.kinds().contains(&kind);
            let met = resolves.needs(kind).is_subset(&facts.available_inputs);
            (listed && met).then(|| (source.clone(), kind))
        })
        .collect()
}

fn source_kind(
    input: &EligibilityInput<'_>,
    view: &EligibilityView,
    facts: &RequestFacts,
    source: &SourceDomain,
) -> Option<ResolveKind> {
    let id = input.provider.descriptor().id();
    let record = view.record(source);
    match input.catalog.owner_of(source) {
        Some(owner) if owner == id => match record {
            None => Some(ResolveKind::Initial),
            Some(record) => {
                owned_record_is_due(input, view, facts, record).then_some(ResolveKind::Refresh)
            }
        },
        // Config validation lets one provider own a source, so another
        // owner means this provider may not resolve it.
        Some(_) => None,
        None => {
            let present =
                record.is_some_and(|record| has_unexpired_id(input.catalog, record, facts.now));
            (!present).then_some(ResolveKind::Initial)
        }
    }
}

/// Whether a stored record of an owned source is due (spec 4.7 step 4).
fn owned_record_is_due(
    input: &EligibilityInput<'_>,
    view: &EligibilityView,
    facts: &RequestFacts,
    record: &KvPartnerId,
) -> bool {
    let id = input.provider.descriptor().id();
    let now = facts.now.as_secs();
    let invalidated = record.w.is_none()
        && record
            .eids
            .iter()
            .flat_map(|entry| &entry.uids)
            .any(|uid| view.has_invalidated_hash(id, &uid_hash(&uid.id)));
    if invalidated {
        return false;
    }
    if record.exp.is_some_and(|exp| now >= exp) || record.w.as_deref() != Some(id.as_str()) {
        return true;
    }
    match record.ra {
        Some(refresh_at) => now >= refresh_at,
        None => record.at.is_none_or(|acquired| {
            acquired.saturating_add(input.settings.refresh_after_sec) <= now
        }),
    }
}

/// Whether KV holds an unexpired ID in this record for an unowned source. A
/// record hidden only by consent or an opt-out still counts: `FillMissing`
/// would not replace it.
fn has_unexpired_id(catalog: &ProviderCatalog, record: &KvPartnerId, now: UnixTime) -> bool {
    if record.eids.is_empty() || record.exp.is_some_and(|exp| now.as_secs() >= exp) {
        return false;
    }
    record.w.as_deref().is_none_or(|writer| {
        writer
            .parse::<IdentityProviderId>()
            .is_ok_and(|writer| catalog.is_configured(&writer))
    })
}

/// What one provider call came to.
#[derive(Debug)]
pub(crate) enum CallOutcome {
    /// The provider returned a resolution.
    Resolved(Resolution),
    /// `request` returned `None`.
    Skipped,
    /// A declared input was unavailable.
    InputUnavailable,
    /// The stored state did not decode, and the resolution the provider
    /// still returned, if any.
    InvalidState(Option<Resolution>),
    /// The hold budget ran out before the answer.
    HoldCutOff,
    /// The call failed because of this visitor's input (a 4xx other than 429,
    /// `Config`, `UndeclaredInput` or `Malformed`).
    InputFailure,
    /// A transport failure, timeout, 429 or 5xx.
    TransportFailure,
    /// The circuit breaker was open, so nothing was sent.
    // Temporary: the breaker, the only producer, arrives in a later task.
    #[allow(dead_code)]
    BreakerOpen,
    /// The burst cap stopped the call.
    BurstCapped,
    /// A limiter error meant nothing was sent.
    NotDispatched,
}

/// What an outcome needs besides the provider and the view.
// Temporary: the request wiring, the first non-test caller, arrives in a later task.
#[allow(dead_code)]
pub(crate) struct OutcomeContext<'a> {
    pub(crate) now: UnixTime,
    pub(crate) catalog: &'a ProviderCatalog,
    pub(crate) settings: &'a ProviderSettings,
    pub(crate) identity: &'a IdentitySettings,
    /// The state revision the call read, for a provider with private state.
    pub(crate) expected_state: Option<StateRevision>,
    /// The consent bits an opt-out records, from [`super::policy::fingerprint_bits`].
    pub(crate) fingerprint: &'a str,
}

/// The stored changes of one outcome.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct OutcomeChanges {
    /// Every removal, and the positive changes of a provider without private
    /// state.
    pub(crate) changes: Vec<PartnerIdChange>,
    /// The positive changes, state change, `n` and `f` of a provider with
    /// private state. They apply as one unit.
    pub(crate) result: Option<ProviderResult>,
    /// `n` and `f` of a provider without a result.
    pub(crate) state: ProviderStateChange,
}

/// The stored changes of an outcome: the table of spec 8, row by row.
// Temporary: the request wiring, the first non-test caller, arrives in a later task.
#[allow(dead_code)]
#[must_use]
pub(crate) fn outcome_to_changes(
    provider: &dyn IdentityProvider,
    outcome: CallOutcome,
    view: &EligibilityView,
    context: &OutcomeContext<'_>,
) -> OutcomeChanges {
    let id = provider.descriptor().id();
    let now = context.now.as_secs();
    match outcome {
        CallOutcome::Resolved(resolution) => resolved_changes(provider, &resolution, view, context),
        CallOutcome::Skipped | CallOutcome::InputUnavailable => {
            wait_until(now.saturating_add(context.settings.refresh_after_sec))
        }
        CallOutcome::InvalidState(resolution) => {
            let mut changes = resolution.map_or_else(OutcomeChanges::default, |resolution| {
                resolved_changes(provider, &resolution, view, context)
            });
            let slot = state_slot(&mut changes);
            slot.st = StateUpdate::Clear;
            slot.f = None;
            if slot.n.is_none() {
                slot.n = Some(now.saturating_add(context.settings.refresh_after_sec));
            }
            changes
        }
        CallOutcome::HoldCutOff => wait_until(now.saturating_add(context.identity.hold_retry_sec)),
        CallOutcome::InputFailure => failure(
            view.failures(id),
            now,
            INPUT_BACKOFF_BASE_SECS,
            INPUT_BACKOFF_CAP_SECS,
        ),
        CallOutcome::TransportFailure => failure(
            view.failures(id),
            now,
            TRANSPORT_BACKOFF_BASE_SECS,
            TRANSPORT_BACKOFF_CAP_SECS,
        ),
        CallOutcome::BurstCapped => wait_until(now.saturating_add(BURST_WAIT_SECS)),
        CallOutcome::BreakerOpen | CallOutcome::NotDispatched => OutcomeChanges::default(),
    }
}

/// The state change the outcome's `st`, `n` and `f` belong to: the result's
/// when there is one, the stand-alone one otherwise.
fn state_slot(changes: &mut OutcomeChanges) -> &mut ProviderStateChange {
    match changes.result.as_mut() {
        Some(result) => &mut result.state,
        None => &mut changes.state,
    }
}

fn wait_until(next: u64) -> OutcomeChanges {
    OutcomeChanges {
        state: ProviderStateChange {
            n: Some(next),
            ..ProviderStateChange::default()
        },
        ..OutcomeChanges::default()
    }
}

/// `base × 2^(failures-1)`, at most `cap`, in seconds.
fn backoff_secs(failures: u32, base: u64, cap: u64) -> u64 {
    base.saturating_mul(2_u64.saturating_pow(failures.saturating_sub(1)))
        .min(cap)
}

fn failure(failures_before: u32, now: u64, base: u64, cap: u64) -> OutcomeChanges {
    let failures = failures_before.saturating_add(1);
    OutcomeChanges {
        state: ProviderStateChange {
            n: Some(now.saturating_add(backoff_secs(failures, base, cap))),
            f: Some(failures),
            ..ProviderStateChange::default()
        },
        ..OutcomeChanges::default()
    }
}

/// Whether the provider declares private state.
fn keeps_private_state(descriptor: &ProviderDescriptor) -> bool {
    descriptor
        .resolves()
        .is_some_and(Resolves::has_private_state)
}

/// How `Resolved(resolution)` becomes changes (spec 4.5 and 8).
fn resolved_changes(
    provider: &dyn IdentityProvider,
    resolution: &Resolution,
    view: &EligibilityView,
    context: &OutcomeContext<'_>,
) -> OutcomeChanges {
    let descriptor = provider.descriptor();
    let id = descriptor.id();
    let now = context.now.as_secs();
    let stateful = keeps_private_state(descriptor);

    let (kept, dropped_unclaimed) = drop_unclaimed(id, resolution.eids(), context.catalog);
    let mut sets = entry_sets(&kept, context.now);
    log_entry_sets(id, &sets);
    let discarded = discarded_sets(descriptor, &mut sets, view);
    let malformed = resolution.state_rejected()
        || reject_positive_part(stateful, &discarded)
        || all_sets_discarded(&sets, &discarded, dropped_unclaimed);
    if malformed {
        log::warn!("Identity provider '{id}' returned a result the coordinator rejects");
    }

    let positive = if malformed {
        Vec::new()
    } else {
        positive_changes(provider, &sets, &discarded, view, context)
    };
    let mut removals = invalidations(id, resolution, view, context.catalog);
    let removed_any = !removals.is_empty();
    let mut state = ProviderStateChange::default();
    if resolution.is_opted_out() {
        removals.extend(opt_out_removals(id, view, context.catalog));
        state.opt_out = Some(KvOptOut {
            at: now,
            fp: context.fingerprint.to_owned(),
        });
        state.st = StateUpdate::Clear;
    }

    let mut next = None;
    if malformed {
        let failures = view.failures(id).saturating_add(1);
        state.f = Some(failures);
        next = Some(now.saturating_add(backoff_secs(
            failures,
            INPUT_BACKOFF_BASE_SECS,
            INPUT_BACKOFF_CAP_SECS,
        )));
    } else {
        if !resolution.eids().is_empty() {
            state.f = Some(0);
        }
        if stateful && state.st == StateUpdate::Keep {
            state.st = state_update(id, resolution);
        }
        if resolution.is_no_match() {
            let wait = resolution
                .retry_in()
                .map_or(NO_MATCH_WAIT_SECS, |delay| delay.as_secs())
                .min(MAX_RETRY_SECS);
            next = Some(now.saturating_add(wait));
        } else if removed_any {
            next = Some(now);
        }
    }
    if let Some(delay) = resolution.retry_in() {
        let earliest = now.saturating_add(delay.as_secs().min(MAX_RETRY_SECS));
        next = Some(next.map_or(earliest, |current| current.max(earliest)));
    }
    state.n = next;

    if stateful && !malformed {
        let result = (!positive.is_empty() || state != ProviderStateChange::default()).then(|| {
            ProviderResult {
                provider: id.clone(),
                ids: positive,
                state,
                expected_state: context.expected_state.clone(),
            }
        });
        return OutcomeChanges {
            changes: removals,
            result,
            state: ProviderStateChange::default(),
        };
    }
    removals.extend(positive);
    OutcomeChanges {
        changes: removals,
        result: None,
        state,
    }
}

/// Step 1: drops the entries of sources the provider does not claim after
/// narrowing. Logs each source with the provider ID only. Also reports
/// whether it dropped anything, which step 5 needs.
fn drop_unclaimed(
    provider: &IdentityProviderId,
    eids: &[ResolvedEid],
    catalog: &ProviderCatalog,
) -> (Vec<ResolvedEid>, bool) {
    let claimed = catalog.sources_of(provider);
    let mut dropped = false;
    let kept = eids
        .iter()
        .filter(|eid| {
            let keep = claimed.contains(eid.source());
            if !keep {
                dropped = true;
                log::debug!(
                    "Dropping an entry of identity provider '{provider}' for unclaimed source '{}'",
                    eid.source()
                );
            }
            keep
        })
        .cloned()
        .collect();
    (kept, dropped)
}

/// Step 2: logs each dropped part and oversized source, never a value.
fn log_entry_sets(provider: &IdentityProviderId, sets: &EntrySets) {
    for drop in &sets.drops {
        log::debug!(
            "Identity provider '{provider}' source '{}': dropped {}",
            drop.source,
            drop.part
        );
    }
    for source in &sets.oversized {
        log::warn!(
            "Identity provider '{provider}' source '{source}': discarded an oversized entry set"
        );
    }
}

/// Step 3: the sources whose set is discarded. An oversized set is. A set of
/// an expiring source that carries no expiry keeps the stored `exp` when its
/// entries equal the stored `eids`, and is discarded otherwise.
fn discarded_sets(
    descriptor: &ProviderDescriptor,
    sets: &mut EntrySets,
    view: &EligibilityView,
) -> BTreeSet<SourceDomain> {
    let mut discarded: BTreeSet<SourceDomain> = sets.oversized.iter().cloned().collect();
    for (source, set) in &mut sets.sets {
        if !descriptor.is_expiring(source) || set.expires_at.is_some() {
            continue;
        }
        let stored_exp = view
            .record(source)
            .filter(|record| record.eids == set.entries)
            .and_then(|record| record.exp);
        match stored_exp {
            Some(exp) => set.expires_at = Some(UnixTime::from_secs(exp)),
            None => {
                log::warn!(
                    "Identity provider '{}' source '{source}': discarded a set that carries no expiry",
                    descriptor.id()
                );
                discarded.insert(source.clone());
            }
        }
    }
    discarded
}

/// Step 4: whether a discarded set rejects the whole positive part. It does
/// for a provider with private state, whose IDs and state apply as a unit.
fn reject_positive_part(stateful: bool, discarded: &BTreeSet<SourceDomain>) -> bool {
    stateful && !discarded.is_empty()
}

/// Step 5: whether the result named a source and every set is discarded,
/// including by dropping entries of unclaimed sources (spec 4.5).
fn all_sets_discarded(
    sets: &EntrySets,
    discarded: &BTreeSet<SourceDomain>,
    dropped_unclaimed: bool,
) -> bool {
    sets.all_discarded()
        || (sets.sets.is_empty() && dropped_unclaimed)
        || (!sets.sets.is_empty() && sets.sets.keys().all(|source| discarded.contains(source)))
}

/// Steps 6 to 8: each kept set becomes one record, written whole.
fn positive_changes(
    provider: &dyn IdentityProvider,
    sets: &EntrySets,
    discarded: &BTreeSet<SourceDomain>,
    view: &EligibilityView,
    context: &OutcomeContext<'_>,
) -> Vec<PartnerIdChange> {
    let descriptor = provider.descriptor();
    let id = descriptor.id();
    let stateful = keeps_private_state(descriptor);
    sets.sets
        .iter()
        .filter(|(source, _)| !discarded.contains(*source))
        .map(|(source, set)| {
            if set.refresh_until.is_some() && !stateful {
                log::warn!(
                    "Identity provider '{id}' source '{source}': ignoring refresh_until without private state"
                );
            }
            let record = record_of(id, set, stateful, context.now);
            if context.catalog.owner_of(source) == Some(id) {
                PartnerIdChange::OwnerWrite {
                    source: source.clone(),
                    record,
                    expected: view.record(source).map(record_revision),
                }
            } else {
                PartnerIdChange::FillMissing {
                    source: source.clone(),
                    record,
                }
            }
        })
        .collect()
}

fn record_of(
    provider: &IdentityProviderId,
    set: &EntrySet,
    stateful: bool,
    now: UnixTime,
) -> KvPartnerId {
    KvPartnerId {
        w: Some(provider.to_string()),
        rv: None,
        at: Some(now.as_secs()),
        exp: set.expires_at.map(UnixTime::as_secs),
        ra: set.refresh_at.map(UnixTime::as_secs),
        ru: stateful
            .then_some(set.refresh_until)
            .flatten()
            .map(UnixTime::as_secs),
        eids: set.entries.clone(),
    }
}

/// Step 9: an `OwnerRemove` for each claimed source the result invalidated
/// that has a record. It carries no hash: applying it hashes the live record.
fn invalidations(
    provider: &IdentityProviderId,
    resolution: &Resolution,
    view: &EligibilityView,
    catalog: &ProviderCatalog,
) -> Vec<PartnerIdChange> {
    let claimed = catalog.sources_of(provider);
    resolution
        .invalidated()
        .iter()
        .filter_map(|source| {
            if !claimed.contains(source) {
                log::debug!(
                    "Ignoring invalidation by identity provider '{provider}' of unclaimed source '{source}'"
                );
                return None;
            }
            Some(PartnerIdChange::OwnerRemove {
                source: source.clone(),
                provider: provider.clone(),
                expected: record_revision(view.record(source)?),
            })
        })
        .collect()
}

/// Step 10: a `WriterRemove` for every record this provider wrote, and for a
/// capture-only provider every record of its sources that has no writer.
fn opt_out_removals(
    provider: &IdentityProviderId,
    view: &EligibilityView,
    catalog: &ProviderCatalog,
) -> Vec<PartnerIdChange> {
    let capture_only = catalog.capture_only_sources(provider);
    view.records()
        .filter(|(source, record)| match record.w.as_deref() {
            Some(writer) => writer == provider.as_str(),
            None => capture_only.contains(source),
        })
        .map(|(source, _)| PartnerIdChange::WriterRemove {
            source: source.clone(),
            writer: provider.clone(),
        })
        .collect()
}

/// The private-state change the resolution asks for.
fn state_update(provider: &IdentityProviderId, resolution: &Resolution) -> StateUpdate {
    match resolution.state_change() {
        StateChange::Keep => StateUpdate::Keep,
        StateChange::Clear => StateUpdate::Clear,
        StateChange::Replace(json) => match serde_json::from_str(json) {
            Ok(value) => StateUpdate::Replace(value),
            Err(_) => {
                log::warn!("Identity provider '{provider}' returned state that is not JSON");
                StateUpdate::Keep
            }
        },
    }
}

/// Everything a dispatch reads that is not per call.
// Temporary: the request wiring, the first non-test caller, arrives in a later task.
#[allow(dead_code)]
pub(crate) struct IdentityDeps<'a> {
    pub(crate) settings: &'a Settings,
    pub(crate) catalog: &'a ProviderCatalog,
    pub(crate) keys: &'a IdentityKeys,
    /// Draws record revisions when a mutation applies.
    // Temporary: first read when the mutations are applied.
    #[allow(dead_code)]
    pub(crate) random: &'a dyn RandomSource,
    pub(crate) services: &'a RuntimeServices,
    pub(crate) rate_limiter: Option<&'a dyn RateLimiter>,
}

/// One provider call to dispatch: the provider and the kind each of its
/// sources needs.
// Temporary: the request wiring, the first non-test caller, arrives in a later task.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct Call {
    pub(crate) provider: IdentityProviderId,
    pub(crate) kinds: BTreeMap<SourceDomain, ResolveKind>,
    /// This provider's stored private state `ps.st`, as JSON. Only this
    /// call's `ResolveInput` sees it.
    pub(crate) state_json: Option<String>,
}

/// A call that was sent and is waiting for its answer.
struct InFlightCall<'a> {
    provider: &'a dyn IdentityProvider,
    input: ResolveInput<'a>,
    call_context: Option<String>,
    /// When the request was sent.
    sent_at: Instant,
    /// How long the backend waits for the first byte.
    timeout: Duration,
    /// The remaining hold budget, not the provider's `timeout_ms`, set the
    /// timeout.
    budget_limited: bool,
}

/// What preparing one call came to.
enum Prepared<'a> {
    /// The call is settled without being sent.
    Settled(CallOutcome),
    /// The call was sent.
    Sent(Box<InFlightCall<'a>>, PlatformPendingRequest),
}

/// Whether the provider is a generic pull provider, the only kind given the
/// full EC ID.
fn is_generic_pull(provider: &dyn IdentityProvider) -> bool {
    provider
        .descriptor()
        .resolves()
        .is_some_and(Resolves::gets_ec_id)
}

/// The order calls are dispatched in. Generic pull providers keep the
/// fairness rule of spec 10.4: sorted by source domain, then rotated left by
/// `(now / 3600) % len`, so the starting partner changes each hour when
/// `max_concurrency` limits a batch. Every other provider follows, by ID.
fn dispatch_order(calls: Vec<Call>, catalog: &ProviderCatalog, now: UnixTime) -> Vec<Call> {
    let (mut generic, mut others): (Vec<Call>, Vec<Call>) = calls.into_iter().partition(|call| {
        catalog
            .get(&call.provider)
            .is_some_and(|provider| is_generic_pull(provider.as_ref()))
    });
    generic.sort_by(|left, right| {
        let source = |call: &Call| {
            catalog
                .get(&call.provider)
                .map(|provider| provider.descriptor().primary_source().clone())
        };
        source(left)
            .cmp(&source(right))
            .then_with(|| left.provider.cmp(&right.provider))
    });
    if !generic.is_empty() {
        let hour = usize::try_from(now.as_secs() / 3600).unwrap_or(0);
        let offset = hour % generic.len();
        generic.rotate_left(offset);
    }
    others.sort_by(|left, right| left.provider.cmp(&right.provider));
    generic.extend(others);
    generic
}

/// Allows an `https` request to a declared host, with no `Cookie` header.
///
/// # Errors
///
/// [`IdentityProviderError::Config`] for any other request.
pub(crate) fn check_request(
    request: &ProviderRequest,
    descriptor: &ProviderDescriptor,
) -> Result<(), Report<IdentityProviderError>> {
    if request.url().scheme() != "https" {
        return Err(IdentityProviderError::config(
            "a provider request must use https",
        ));
    }
    let host = request.url().host_str().unwrap_or_default();
    let declared = descriptor.resolves().is_some_and(|resolves| {
        resolves
            .hosts()
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(host))
    });
    if !declared {
        return Err(IdentityProviderError::config(
            "a provider request must go to a declared host",
        ));
    }
    if request.has_cookie_header() {
        return Err(IdentityProviderError::config(
            "a provider request must not set a Cookie header",
        ));
    }
    Ok(())
}

/// Decodes a response body to at most `cap` bytes: identity, `gzip` or `br`.
///
/// `None` when the decoded body is over `cap`, does not decode, or the
/// encoding is unknown. The cap applies while decoding, so a small body
/// cannot expand past it.
pub(crate) fn decode_body(encoding: Option<&str>, body: Vec<u8>, cap: usize) -> Option<Vec<u8>> {
    let compression = match encoding.map(str::trim) {
        None | Some("") => Compression::None,
        Some(name) if name.eq_ignore_ascii_case("identity") => Compression::None,
        Some(name) if name.eq_ignore_ascii_case("gzip") => Compression::Gzip,
        Some(name) if name.eq_ignore_ascii_case("br") => Compression::Brotli,
        Some(_) => return None,
    };
    if compression == Compression::None {
        return (body.len() <= cap).then_some(body);
    }
    let mut decoder = BodyStreamDecoder::new(compression, cap);
    let mut decoded = decoder
        .decode_chunk(bytes::Bytes::from(body))
        .ok()?
        .to_vec();
    decoded.extend(decoder.finish().ok()?);
    (decoded.len() <= cap).then_some(decoded)
}

/// Whether the response declares a body over `cap`, or an unreadable length.
pub(crate) fn content_length_exceeds_limit(
    response: &PlatformResponse,
    cap: usize,
    label: &str,
) -> bool {
    let Some(value) = response.response.headers().get(header::CONTENT_LENGTH) else {
        return false;
    };
    let Some(length) = value
        .to_str()
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    else {
        log::warn!("'{label}' returned an unreadable Content-Length header, rejecting");
        return true;
    };
    if length > cap {
        log::warn!("'{label}' returned an oversized Content-Length ({length} bytes), rejecting");
        return true;
    }
    false
}

/// Buffers a response body of at most `cap` bytes, as sent (still encoded).
/// `None` when the declared or actual length is over `cap`.
pub(crate) fn buffered_body(
    response: PlatformResponse,
    cap: usize,
    label: &str,
) -> Option<(http::response::Parts, bytes::Bytes)> {
    if content_length_exceeds_limit(&response, cap, label) {
        return None;
    }
    let (parts, body) = response.response.into_parts();
    let body = body.into_bytes().unwrap_or_default();
    if body.len() > cap {
        log::warn!(
            "'{label}' returned an oversized response ({} bytes), rejecting",
            body.len()
        );
        return None;
    }
    Some((parts, body))
}

/// What waiting for one in-flight request came to.
pub(crate) enum Waited {
    /// The response arrived.
    Response(PlatformResponse),
    /// The request failed or timed out.
    Failed,
    /// The caller stopped waiting, so the request was dropped.
    Dropped,
}

/// Waits for each in-flight request in turn and empties `in_flight`. All were
/// sent up front, so waiting in turn does not change concurrency.
///
/// `keep_waiting` is asked before each wait. Once it returns `false`, every
/// request still pending is dropped without waiting.
pub(crate) fn drain_in_flight<T: core::fmt::Display>(
    in_flight: &mut Vec<(T, PlatformPendingRequest)>,
    services: &RuntimeServices,
    mut keep_waiting: impl FnMut() -> bool,
) -> Vec<(T, Waited)> {
    let mut waiting = true;
    in_flight
        .drain(..)
        .map(|(label, pending)| {
            waiting = waiting && keep_waiting();
            if !waiting {
                return (label, Waited::Dropped);
            }
            match futures::executor::block_on(services.http_client().wait(pending)) {
                Ok(response) => (label, Waited::Response(response)),
                Err(error) => {
                    log::warn!("Request failed for '{label}': {error:?}");
                    (label, Waited::Failed)
                }
            }
        })
        .collect()
}

/// Logs a provider `Config` failure. Only the error's own context is logged,
/// never its attachments.
fn log_config(provider: &IdentityProviderId, error: &Report<IdentityProviderError>) {
    log::error!(
        "Identity provider '{provider}' configuration error: {}",
        error.current_context()
    );
}

/// The outcome a `request` error becomes.
fn request_error_outcome(
    provider: &IdentityProviderId,
    error: &Report<IdentityProviderError>,
) -> CallOutcome {
    if matches!(
        error.current_context(),
        IdentityProviderError::InputUnavailable { .. }
    ) {
        return CallOutcome::InputUnavailable;
    }
    log_config(provider, error);
    CallOutcome::InputFailure
}

/// The outcome a `response` error becomes.
fn response_error_outcome(error: &Report<IdentityProviderError>) -> CallOutcome {
    match error.current_context() {
        IdentityProviderError::Rejected { status }
            if *status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() =>
        {
            CallOutcome::TransportFailure
        }
        IdentityProviderError::InvalidState => CallOutcome::InvalidState(None),
        _ => CallOutcome::InputFailure,
    }
}

/// The first-byte timeout of a call, or `None` when a hold dispatch has no
/// budget left.
fn call_timeout(
    deps: &IdentityDeps<'_>,
    settings: &ProviderSettings,
    remaining: Option<Duration>,
) -> Option<(Duration, bool)> {
    let policy = deps.services.backend().naming_policy();
    let millis = |value: u64| u32::try_from(value).unwrap_or(u32::MAX);
    let mut budget_limited = false;
    let timeout_ms = match remaining {
        None => {
            let configured = millis(settings.after_send_timeout_ms);
            policy.canonicalize_transport_timeout_ms(configured, configured)
        }
        Some(remaining) => {
            let configured = millis(
                settings
                    .timeout_ms
                    .unwrap_or(deps.settings.ec.identity.hold_budget_ms),
            );
            let remaining_ms = u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX);
            budget_limited = remaining_ms < configured;
            policy.canonicalize_transport_timeout_ms(remaining_ms, configured)
        }
    };
    (timeout_ms > 0).then(|| (Duration::from_millis(u64::from(timeout_ms)), budget_limited))
}

/// Whether the burst cap stops the call. `Err` when it could not be read.
fn burst_capped(
    deps: &IdentityDeps<'_>,
    provider: &IdentityProviderId,
    ec_id: Option<&str>,
    settings: &ProviderSettings,
) -> Result<bool, ()> {
    let Some(limiter) = deps.rate_limiter else {
        return Ok(false);
    };
    let Some(ec_id) = ec_id else {
        log::debug!("Identity provider '{provider}' not dispatched: the request has no EC ID");
        return Err(());
    };
    let key = format!("identity:{provider}:{}", visitor_key(deps.keys, ec_id));
    limiter
        .exceeded_per_minute(&key, settings.max_calls_per_minute)
        .map_err(|error| {
            log::warn!(
                "Identity provider '{provider}' not dispatched, burst cap unreadable: {error:?}"
            );
        })
}

/// Builds, checks and sends one call.
fn prepare_call<'a>(
    deps: &IdentityDeps<'a>,
    call: &'a Call,
    signals: &'a ResolveSignals<'a>,
    started: Instant,
    budget: Option<Duration>,
) -> Prepared<'a> {
    let Some(provider) = deps.catalog.get(&call.provider) else {
        log::error!(
            "Identity provider '{}' is not in the catalog, not dispatched",
            call.provider
        );
        return Prepared::Settled(CallOutcome::InputFailure);
    };
    let provider: &dyn IdentityProvider = provider.as_ref();
    let descriptor = provider.descriptor();
    let id = descriptor.id();
    let settings = deps.settings.ec.identity.provider(id);

    let input = ResolveInput::new(descriptor, call.kinds.clone(), signals)
        .with_state_json(call.state_json.clone());
    let request = provider.request(&input);
    if !input.recorded_errors().is_empty() {
        log::error!("Identity provider '{id}' read an input it did not declare");
        return Prepared::Settled(CallOutcome::InputFailure);
    }
    let request = match request {
        Ok(Some(request)) => request,
        Ok(None) => return Prepared::Settled(CallOutcome::Skipped),
        Err(error) => return Prepared::Settled(request_error_outcome(id, &error)),
    };
    if let Err(error) = check_request(&request, descriptor) {
        log_config(id, &error);
        return Prepared::Settled(CallOutcome::InputFailure);
    }

    let remaining = budget.map(|budget| budget.saturating_sub(started.elapsed()));
    let Some((timeout, budget_limited)) = call_timeout(deps, &settings, remaining) else {
        return Prepared::Settled(CallOutcome::HoldCutOff);
    };
    match burst_capped(deps, id, signals.ec_id.as_deref(), &settings) {
        Ok(true) => return Prepared::Settled(CallOutcome::BurstCapped),
        Ok(false) => {}
        Err(()) => return Prepared::Settled(CallOutcome::NotDispatched),
    }

    let url = request.url();
    let backend = deps.services.backend().ensure(&PlatformBackendSpec {
        scheme: url.scheme().to_owned(),
        host: url.host_str().unwrap_or_default().to_owned(),
        port: url.port(),
        host_header_override: None,
        certificate_check: deps.settings.proxy.certificate_check,
        first_byte_timeout: timeout,
        between_bytes_timeout: timeout,
        discriminator: Some(id.to_string()),
    });
    let backend = match backend {
        Ok(name) => name,
        Err(error) => {
            log::warn!("Identity provider '{id}': failed to resolve the backend: {error:?}");
            return Prepared::Settled(CallOutcome::TransportFailure);
        }
    };

    let mut builder = http::Request::builder()
        .method(request.method().clone())
        .uri(url.as_str());
    for (name, value) in request.headers() {
        builder = builder.header(name, value);
    }
    let body = if request.body().is_empty() {
        EdgeBody::empty()
    } else {
        EdgeBody::from(request.body().to_vec())
    };
    let Ok(outbound) = builder.body(body) else {
        log::error!("Identity provider '{id}': the request could not be built");
        return Prepared::Settled(CallOutcome::InputFailure);
    };
    let call_context = request.call_context_json().map(str::to_owned);

    match futures::executor::block_on(
        deps.services
            .http_client()
            .send_async(PlatformHttpRequest::new(outbound, backend)),
    ) {
        Ok(pending) => Prepared::Sent(
            Box::new(InFlightCall {
                provider,
                input,
                call_context,
                sent_at: Instant::now(),
                timeout,
                budget_limited,
            }),
            pending,
        ),
        Err(error) => {
            log::warn!("Identity provider '{id}': failed to send: {error:?}");
            Prepared::Settled(CallOutcome::TransportFailure)
        }
    }
}

/// The outcome of a call whose response arrived.
fn finish_call(
    call: &InFlightCall<'_>,
    response: PlatformResponse,
    max_response_bytes: usize,
) -> CallOutcome {
    let id = call.provider.descriptor().id();
    let Some((parts, raw)) = buffered_body(response, max_response_bytes, id.as_str()) else {
        return CallOutcome::TransportFailure;
    };
    let encoding = parts
        .headers
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok());
    let Some(body) = decode_body(encoding, raw.to_vec(), max_response_bytes) else {
        log::warn!("Identity provider '{id}' returned a body that is over the cap or undecodable");
        return CallOutcome::TransportFailure;
    };
    let response =
        ProviderResponse::new(parts.status, parts.headers, body, call.call_context.clone());
    let result = call.provider.response(&response, &call.input);
    if !call.input.recorded_errors().is_empty() {
        log::error!("Identity provider '{id}' read an input it did not declare");
        return CallOutcome::InputFailure;
    }
    match result {
        Ok(resolution) if call.input.state_decode_failed() => {
            CallOutcome::InvalidState(Some(resolution))
        }
        Ok(resolution) => CallOutcome::Resolved(resolution),
        Err(error) => {
            if matches!(
                error.current_context(),
                IdentityProviderError::Config { .. }
            ) {
                log_config(id, &error);
            }
            response_error_outcome(&error)
        }
    }
}

/// Dispatches `calls` and returns each provider's outcome (spec 4.7).
///
/// At most `max_concurrency` calls are in flight. `budget` is `Some(the
/// remaining hold budget)` for a hold dispatch before the auction, and
/// `None` after the response is sent: there, a call that misses its timeout is
/// an ordinary transport failure.
// Temporary: the request wiring, the first non-test caller, arrives in a later task.
#[allow(dead_code)]
pub(crate) fn dispatch(
    deps: &IdentityDeps<'_>,
    calls: Vec<Call>,
    signals: &ResolveSignals<'_>,
    budget: Option<Duration>,
) -> Vec<(IdentityProviderId, CallOutcome)> {
    let started = Instant::now();
    let identity = &deps.settings.ec.identity;
    let max_concurrency = identity.max_concurrency.max(1);
    let calls = dispatch_order(calls, deps.catalog, signals.now);
    let mut outcomes = Vec::with_capacity(calls.len());
    let mut in_flight: Vec<(IdentityProviderId, PlatformPendingRequest)> = Vec::new();
    let mut sent: BTreeMap<IdentityProviderId, Box<InFlightCall<'_>>> = BTreeMap::new();

    let drain = |in_flight: &mut Vec<(IdentityProviderId, PlatformPendingRequest)>,
                 sent: &mut BTreeMap<IdentityProviderId, Box<InFlightCall<'_>>>,
                 outcomes: &mut Vec<(IdentityProviderId, CallOutcome)>| {
        let within_budget = || budget.is_none_or(|budget| started.elapsed() < budget);
        for (id, waited) in drain_in_flight(in_flight, deps.services, within_budget) {
            let Some(call) = sent.remove(&id) else {
                continue;
            };
            let outcome = match waited {
                Waited::Response(response) => {
                    finish_call(&call, response, identity.max_response_bytes)
                }
                // A failed wait is a cut off only when the hold budget was
                // spent: the whole budget, or a timeout the remaining budget
                // set. Any other failure counts against the provider.
                Waited::Failed
                    if budget.is_some_and(|budget| started.elapsed() >= budget)
                        || (budget.is_some()
                            && call.budget_limited
                            && call.sent_at.elapsed() >= call.timeout) =>
                {
                    CallOutcome::HoldCutOff
                }
                Waited::Failed => CallOutcome::TransportFailure,
                Waited::Dropped => CallOutcome::HoldCutOff,
            };
            outcomes.push((id, outcome));
        }
    };

    for call in &calls {
        match prepare_call(deps, call, signals, started, budget) {
            Prepared::Settled(outcome) => outcomes.push((call.provider.clone(), outcome)),
            Prepared::Sent(sent_call, pending) => {
                sent.insert(call.provider.clone(), sent_call);
                in_flight.push((call.provider.clone(), pending));
                if in_flight.len() >= max_concurrency {
                    drain(&mut in_flight, &mut sent, &mut outcomes);
                }
            }
        }
    }
    drain(&mut in_flight, &mut sent, &mut outcomes);
    outcomes
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    use std::io::Write as _;

    use serde_json::json;

    use super::*;
    use crate::consent::types::GppConsent;
    use crate::consent::us_privacy::decode_us_privacy;
    use crate::ec::identity::descriptor::{
        ConsentPolicy, Input, ProviderDescriptor, ResolveKind, Resolves, UsOptOut,
    };
    use crate::ec::identity::input::{ProviderConsent, SeededRandom, ThreadRandom};
    use crate::ec::identity::policy::fingerprint_bits;
    use crate::ec::identity::resolution::{ResolvedEid, ResolvedUid};
    use crate::ec::identity::revision::{record_revision, uid_hash};
    use crate::ec::identity::test_support::fake_http::{
        FakeBackend, FakeHttpClient, ScriptedProvider, fake_services,
    };
    use crate::ec::identity::test_support::{
        OutcomeFixture, allowing_consent, facts_for_writer, fixed_capture_provider, fixed_provider,
        pid, provider_from, record_for_test, source, stateful_fixed_provider,
        test_consent_gdpr_without_vendor,
    };
    use crate::ec::kv_types::{KvEid, KvUid};
    use crate::settings::ProviderMode;
    use crate::test_support::tests::create_test_settings;

    /// The six lines every eligibility test repeats: one provider, its
    /// catalog and the settings and consent the call reads.
    struct EligibilityFixture {
        provider: Arc<dyn IdentityProvider>,
        catalog: ProviderCatalog,
        settings: ProviderSettings,
        identity: IdentitySettings,
        consent: ConsentContext,
    }

    impl EligibilityFixture {
        /// A catalog holding `provider` alone, with no source owners.
        fn new(provider: &Arc<dyn IdentityProvider>) -> Self {
            Self::with_catalog(
                provider,
                ProviderCatalog::new(vec![provider.clone()], BTreeMap::new()),
            )
        }

        /// A catalog holding `provider` alone, owning each of `sources`.
        fn owning(provider: &Arc<dyn IdentityProvider>, sources: &[&str]) -> Self {
            let owners = sources
                .iter()
                .map(|name| (source(name), provider.descriptor().id().clone()))
                .collect();
            Self::with_catalog(
                provider,
                ProviderCatalog::new(vec![provider.clone()], owners),
            )
        }

        fn with_catalog(provider: &Arc<dyn IdentityProvider>, catalog: ProviderCatalog) -> Self {
            Self {
                provider: provider.clone(),
                catalog,
                settings: ProviderSettings::default(),
                identity: IdentitySettings::default(),
                consent: allowing_consent(),
            }
        }

        fn with_settings(mut self, settings: ProviderSettings) -> Self {
            self.settings = settings;
            self
        }

        fn with_consent(mut self, consent: ConsentContext) -> Self {
            self.consent = consent;
            self
        }

        fn check(&self, view: &EligibilityView, facts: &RequestFacts, phase: Phase) -> Eligibility {
            let input = EligibilityInput {
                provider: self.provider.as_ref(),
                settings: &self.settings,
                identity: &self.identity,
                catalog: &self.catalog,
                consent: &self.consent,
            };
            eligibility(&input, view, facts, phase)
        }

        fn is_eligible(&self, view: &EligibilityView, facts: &RequestFacts, phase: Phase) -> bool {
            matches!(self.check(view, facts, phase), Eligibility::Eligible { .. })
        }

        /// The kinds of a writer navigation, after send.
        fn kinds(&self, view: &EligibilityView) -> BTreeMap<SourceDomain, ResolveKind> {
            match self.check(view, &facts_for_writer(), Phase::AfterSend) {
                Eligibility::Eligible { kinds } => kinds,
                Eligibility::Skip(reason) => panic!("should be eligible, skipped for {reason:?}"),
            }
        }

        fn skip_reason(&self, view: &EligibilityView, facts: &RequestFacts) -> SkipReason {
            match self.check(view, facts, Phase::AfterSend) {
                Eligibility::Skip(reason) => reason,
                Eligibility::Eligible { .. } => panic!("should be skipped"),
            }
        }
    }

    fn custom_provider(
        id: &str,
        sources: &[&str],
        resolves: Resolves,
        policy: ConsentPolicy,
    ) -> Arc<dyn IdentityProvider> {
        let mut builder = ProviderDescriptor::builder(pid(id))
            .with_resolves(resolves.with_endpoint("https://api.example.com"))
            .with_consent_policy(policy);
        for name in sources {
            builder = builder.with_source(source(name));
        }
        provider_from(builder.build().expect("should build the descriptor"))
    }

    fn expiring_provider(id: &str, sources: &[&str]) -> Arc<dyn IdentityProvider> {
        let mut builder = ProviderDescriptor::builder(pid(id))
            .with_resolves(Resolves::new().with_endpoint("https://api.example.com"))
            .with_consent_policy(ConsentPolicy::eid_persistence_only())
            .with_expiring_ids();
        for name in sources {
            builder = builder.with_source(source(name));
        }
        provider_from(builder.build().expect("should build the descriptor"))
    }

    fn positive_record(change: &PartnerIdChange) -> Option<&KvPartnerId> {
        match change {
            PartnerIdChange::FillMissing { record, .. }
            | PartnerIdChange::OwnerWrite { record, .. } => Some(record),
            PartnerIdChange::OwnerRemove { .. } | PartnerIdChange::WriterRemove { .. } => None,
        }
    }

    fn kinds_of(entries: &[(&str, ResolveKind)]) -> BTreeMap<SourceDomain, ResolveKind> {
        entries
            .iter()
            .map(|(name, kind)| (source(name), *kind))
            .collect()
    }

    fn one_entry(uid: &str) -> KvEid {
        KvEid {
            uids: vec![KvUid {
                id: uid.to_owned(),
                atype: None,
                ext: None,
            }],
            inserter: None,
            matcher: None,
            mm: None,
            ext: None,
        }
    }

    fn large_entries(source_domain: &SourceDomain) -> Vec<ResolvedEid> {
        // Three entries of about 2 KB each: the set is over MAX_ENTRY_SET_BYTES.
        let large_ext = json!({ "segments": "x".repeat(2_000) });
        (0..3)
            .map(|index| {
                ResolvedEid::new(source_domain, ResolvedUid::new(format!("example-{index}")))
                    .with_ext(large_ext.clone())
            })
            .collect()
    }

    // Eligibility

    #[test]
    fn skips_when_not_a_real_browser() {
        let fixture = EligibilityFixture::new(&fixed_provider("p", &["a.example.com"]));
        let facts = RequestFacts {
            is_real_browser: false,
            ..facts_for_writer()
        };

        assert_eq!(
            fixture.skip_reason(&EligibilityView::empty(), &facts),
            SkipReason::NotRealBrowser,
            "should skip a client that does not look like a browser"
        );
        assert!(
            fixture.is_eligible(
                &EligibilityView::empty(),
                &facts_for_writer(),
                Phase::AfterSend
            ),
            "control: a real browser should be eligible"
        );
    }

    #[test]
    fn skips_when_ec_persistence_is_not_allowed() {
        let fixture = EligibilityFixture::new(&fixed_provider("p", &["a.example.com"]));
        let facts = RequestFacts {
            ec_persistence_allowed: false,
            ..facts_for_writer()
        };

        assert_eq!(
            fixture.skip_reason(&EligibilityView::empty(), &facts),
            SkipReason::EcPersistenceNotAllowed,
            "should skip when global consent blocks EID persistence"
        );
    }

    #[test]
    fn skips_when_the_consent_policy_blocks() {
        let provider = custom_provider(
            "p",
            &["a.example.com"],
            Resolves::new(),
            ConsentPolicy::strict().with_tcf_vendor(5),
        );
        let blocked =
            EligibilityFixture::new(&provider).with_consent(test_consent_gdpr_without_vendor(5));
        let allowed =
            EligibilityFixture::new(&provider).with_consent(test_consent_gdpr_without_vendor(6));
        let view = EligibilityView::empty();

        assert_eq!(
            blocked.skip_reason(&view, &facts_for_writer()),
            SkipReason::ConsentPolicy,
            "should skip when the policy's vendor has no consent"
        );
        assert!(
            allowed.is_eligible(&view, &facts_for_writer(), Phase::AfterSend),
            "control: should run when the vendor has consent"
        );
    }

    #[test]
    fn skips_a_provider_that_does_not_resolve() {
        let fixture = EligibilityFixture::new(&fixed_capture_provider("p", &["a.example.com"]));

        assert_eq!(
            fixture.skip_reason(&EligibilityView::empty(), &facts_for_writer()),
            SkipReason::NotAResolver,
            "should never run a capture-only provider"
        );
    }

    #[test]
    fn skips_new_visitor_in_every_mode() {
        let fixture = EligibilityFixture::new(&fixed_provider("p", &["a.example.com"]));
        let view = EligibilityView::empty();
        for phase in [Phase::Hold, Phase::AfterSend] {
            let facts = RequestFacts {
                new_visitor: true,
                ..facts_for_writer()
            };
            assert!(
                matches!(
                    fixture.check(&view, &facts, phase),
                    Eligibility::Skip(SkipReason::NewVisitor)
                ),
                "should run no provider on a new visitor's first navigation ({phase:?})"
            );
        }
        let returning = facts_for_writer();
        assert!(
            matches!(
                fixture.check(&view, &returning, Phase::AfterSend),
                Eligibility::Eligible { .. }
            ),
            "control: a returning writer should be eligible"
        );
    }

    #[test]
    fn stager_runs_no_provider_in_either_phase() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let view = EligibilityView::empty();
        let stager = RequestFacts {
            writer: false,
            runs_auction: true,
            ..facts_for_writer()
        };
        for mode in [ProviderMode::Hold, ProviderMode::AfterSend] {
            let fixture = EligibilityFixture::new(&provider).with_settings(ProviderSettings {
                mode,
                ..ProviderSettings::default()
            });
            for phase in [Phase::Hold, Phase::AfterSend] {
                assert!(
                    matches!(
                        fixture.check(&view, &stager, phase),
                        Eligibility::Skip(SkipReason::NotWriter)
                    ),
                    "should run no provider on a stager ({mode:?}, {phase:?})"
                );
            }
        }
    }

    #[test]
    fn owned_record_written_by_someone_else_is_due_for_refresh() {
        let fixture = EligibilityFixture::owning(
            &fixed_provider("p", &["a.example.com", "b.example.com"]),
            &["a.example.com", "b.example.com"],
        );
        let mut view = EligibilityView::empty();
        // Written by the browser, and acquired recently.
        view.set_record_for_test(
            &source("a.example.com"),
            record_for_test("example-a", None, 999),
        );
        // Written by this provider, acquired recently.
        view.set_record_for_test(
            &source("b.example.com"),
            record_for_test("example-b", Some("p"), 999),
        );

        assert_eq!(
            fixture.kinds(&view),
            kinds_of(&[("a.example.com", ResolveKind::Refresh)]),
            "should refresh the browser-seeded record and leave the fresh owned one"
        );
    }

    #[test]
    fn owned_source_without_a_record_is_initial() {
        let fixture = EligibilityFixture::owning(
            &fixed_provider("p", &["a.example.com"]),
            &["a.example.com"],
        );

        assert_eq!(
            fixture.kinds(&EligibilityView::empty()),
            kinds_of(&[("a.example.com", ResolveKind::Initial)]),
            "should ask for an initial resolution when there is no record"
        );
    }

    #[test]
    fn owned_record_is_due_by_refresh_anchor_or_age() {
        let fixture = EligibilityFixture::owning(
            &fixed_provider("p", &["a.example.com", "b.example.com", "c.example.com"]),
            &["a.example.com", "b.example.com", "c.example.com"],
        )
        .with_settings(ProviderSettings {
            refresh_after_sec: 100,
            ..ProviderSettings::default()
        });
        let mut view = EligibilityView::empty();
        // `ra` has passed although `at` is recent.
        let mut passed = record_for_test("example-a", Some("p"), 999);
        passed.ra = Some(1_000);
        view.set_record_for_test(&source("a.example.com"), passed);
        // `ra` is ahead although `at` is old, so `ra` wins.
        let mut ahead = record_for_test("example-b", Some("p"), 100);
        ahead.ra = Some(5_000);
        view.set_record_for_test(&source("b.example.com"), ahead);
        // No `ra`, and `at + refresh_after_sec` has passed.
        view.set_record_for_test(
            &source("c.example.com"),
            record_for_test("example-c", Some("p"), 900),
        );

        assert_eq!(
            fixture.kinds(&view),
            kinds_of(&[
                ("a.example.com", ResolveKind::Refresh),
                ("c.example.com", ResolveKind::Refresh)
            ]),
            "should refresh when ra passed, or when there is no ra and the age passed"
        );
    }

    #[test]
    fn owned_record_without_at_or_ra_is_due() {
        let fixture = EligibilityFixture::owning(
            &fixed_provider("p", &["a.example.com"]),
            &["a.example.com"],
        );
        let mut view = EligibilityView::empty();
        let mut record = record_for_test("example-a", Some("p"), 999);
        record.at = None;
        view.set_record_for_test(&source("a.example.com"), record);

        assert_eq!(
            fixture.kinds(&view),
            kinds_of(&[("a.example.com", ResolveKind::Refresh)]),
            "should refresh a record with no acquisition time"
        );
    }

    #[test]
    fn browser_seeded_uid_in_iv_is_never_due() {
        let fixture = EligibilityFixture::owning(
            &fixed_provider("p", &["a.example.com"]),
            &["a.example.com"],
        );
        let mut view = EligibilityView::empty();
        view.set_record_for_test(
            &source("a.example.com"),
            record_for_test("example-bad", None, 10),
        );

        assert_eq!(
            fixture.kinds(&view),
            kinds_of(&[("a.example.com", ResolveKind::Refresh)]),
            "control: a browser-seeded record should be due"
        );

        view.set_invalidated_for_test("p", vec![uid_hash("example-bad")]);
        assert!(
            matches!(
                fixture.check(&view, &facts_for_writer(), Phase::AfterSend),
                Eligibility::Skip(SkipReason::NoSourceDue)
            ),
            "should never refresh a browser-seeded value the provider invalidated"
        );
    }

    #[test]
    fn expired_refresh_anchor_stays_refresh() {
        let fixture = EligibilityFixture::owning(
            &expiring_provider("p", &["a.example.com"]),
            &["a.example.com"],
        );
        let mut view = EligibilityView::empty();
        let mut anchor = record_for_test("example-a", Some("p"), 100);
        anchor.exp = Some(500);
        anchor.ru = Some(9_000);
        view.set_record_for_test(&source("a.example.com"), anchor);

        assert_eq!(
            fixture.kinds(&view),
            kinds_of(&[("a.example.com", ResolveKind::Refresh)]),
            "should keep the kind Refresh for an expired record that is still stored"
        );
    }

    #[test]
    fn unowned_source_with_unexpired_record_is_not_initial_even_when_consent_hides_it() {
        let hiding = custom_provider(
            "q",
            &["a.example.com"],
            Resolves::new(),
            ConsentPolicy::strict().with_tcf_vendor(5),
        );
        let asking = fixed_provider("p", &["a.example.com"]);
        let catalog = ProviderCatalog::new(vec![hiding, asking.clone()], BTreeMap::new());
        // Consent withholds vendor 5, so `q`'s record is not servable.
        let fixture = EligibilityFixture::with_catalog(&asking, catalog)
            .with_consent(test_consent_gdpr_without_vendor(5));
        let mut view = EligibilityView::empty();
        view.set_record_for_test(
            &source("a.example.com"),
            record_for_test("example-a", Some("q"), 900),
        );

        assert!(
            matches!(
                fixture.check(&view, &facts_for_writer(), Phase::AfterSend),
                Eligibility::Skip(SkipReason::NoSourceDue)
            ),
            "should not call to fill a source that already holds an unexpired ID"
        );
        assert_eq!(
            fixture.kinds(&EligibilityView::empty()),
            kinds_of(&[("a.example.com", ResolveKind::Initial)]),
            "control: an absent record should be Initial"
        );
    }

    #[test]
    fn expired_unowned_record_counts_as_absent() {
        let fixture = EligibilityFixture::new(&fixed_provider("p", &["a.example.com"]));
        let mut view = EligibilityView::empty();
        let mut record = record_for_test("example-a", None, 100);
        record.exp = Some(1_000);
        view.set_record_for_test(&source("a.example.com"), record);

        assert_eq!(
            fixture.kinds(&view),
            kinds_of(&[("a.example.com", ResolveKind::Initial)]),
            "should treat a record expired at now as absent"
        );
    }

    #[test]
    fn record_of_an_unconfigured_writer_counts_as_absent() {
        let fixture = EligibilityFixture::new(&fixed_provider("p", &["a.example.com"]));
        let mut view = EligibilityView::empty();
        view.set_record_for_test(
            &source("a.example.com"),
            record_for_test("example-a", Some("gone"), 900),
        );

        assert_eq!(
            fixture.kinds(&view),
            kinds_of(&[("a.example.com", ResolveKind::Initial)]),
            "should treat a record written by an unconfigured provider as absent"
        );

        let mut browser_view = EligibilityView::empty();
        browser_view.set_record_for_test(
            &source("a.example.com"),
            record_for_test("example-a", None, 900),
        );
        assert!(
            matches!(
                fixture.check(&browser_view, &facts_for_writer(), Phase::AfterSend),
                Eligibility::Skip(SkipReason::NoSourceDue)
            ),
            "control: a browser-seeded unexpired record should count as present"
        );
    }

    #[test]
    fn hold_provider_runs_before_the_auction_and_not_after_send() {
        let fixture = EligibilityFixture::new(&fixed_provider("p", &["a.example.com"]))
            .with_settings(ProviderSettings {
                mode: ProviderMode::Hold,
                ..ProviderSettings::default()
            });
        let view = EligibilityView::empty();
        let facts = RequestFacts {
            runs_auction: true,
            ..facts_for_writer()
        };

        assert!(
            fixture.is_eligible(&view, &facts, Phase::Hold),
            "should hold before the auction"
        );
        assert!(
            !fixture.is_eligible(&view, &facts, Phase::AfterSend),
            "should not run again after send"
        );
    }

    #[test]
    fn hold_provider_runs_after_send_when_the_navigation_runs_no_auction() {
        let fixture = EligibilityFixture::new(&fixed_provider("p", &["a.example.com"]))
            .with_settings(ProviderSettings {
                mode: ProviderMode::Hold,
                ..ProviderSettings::default()
            });
        let view = EligibilityView::empty();

        let no_auction = RequestFacts {
            runs_auction: false,
            ..facts_for_writer()
        };
        let auction = RequestFacts {
            runs_auction: true,
            ..facts_for_writer()
        };
        let stager = RequestFacts {
            writer: false,
            runs_auction: true,
            ..facts_for_writer()
        };

        assert!(
            fixture.is_eligible(&view, &no_auction, Phase::AfterSend),
            "should move after send without an auction"
        );
        assert!(
            !fixture.is_eligible(&view, &no_auction, Phase::Hold),
            "should not hold without an auction"
        );
        assert!(
            fixture.is_eligible(&view, &auction, Phase::Hold),
            "control: should hold before an auction"
        );
        assert!(
            !fixture.is_eligible(&view, &auction, Phase::AfterSend),
            "should not also run after send"
        );
        assert!(
            !fixture.is_eligible(&view, &stager, Phase::Hold),
            "should never run on a stager"
        );
    }

    #[test]
    fn after_send_provider_never_runs_in_hold() {
        let fixture = EligibilityFixture::new(&fixed_provider("p", &["a.example.com"]));
        let view = EligibilityView::empty();
        for runs_auction in [true, false] {
            let facts = RequestFacts {
                runs_auction,
                ..facts_for_writer()
            };
            assert!(
                !fixture.is_eligible(&view, &facts, Phase::Hold),
                "should not hold an after-send provider (auction: {runs_auction})"
            );
            assert!(
                fixture.is_eligible(&view, &facts, Phase::AfterSend),
                "control: should run it after send (auction: {runs_auction})"
            );
        }
    }

    #[test]
    fn narrowed_away_source_is_never_a_kind() {
        let provider = fixed_provider("p", &["a.example.com", "b.example.com"]);
        let catalog = ProviderCatalog::new(vec![provider.clone()], BTreeMap::new())
            .with_narrowing(BTreeMap::from([(pid("p"), vec![source("a.example.com")])]));
        let fixture = EligibilityFixture::with_catalog(&provider, catalog);

        assert_eq!(
            fixture.kinds(&EligibilityView::empty()),
            kinds_of(&[("a.example.com", ResolveKind::Initial)]),
            "should leave out a source the operator narrowed away"
        );
    }

    #[test]
    fn kinds_the_provider_does_not_list_are_left_out() {
        let provider = custom_provider(
            "p",
            &["a.example.com", "b.example.com"],
            Resolves::new().with_kinds([ResolveKind::Initial]),
            ConsentPolicy::eid_persistence_only(),
        );
        let fixture = EligibilityFixture::owning(&provider, &["a.example.com", "b.example.com"]);
        let mut view = EligibilityView::empty();
        view.set_record_for_test(
            &source("a.example.com"),
            record_for_test("example-a", None, 10),
        );

        assert_eq!(
            fixture.kinds(&view),
            kinds_of(&[("b.example.com", ResolveKind::Initial)]),
            "should leave out the Refresh source of an Initial-only provider"
        );
    }

    #[test]
    fn source_whose_needs_are_unavailable_is_left_out() {
        let provider = custom_provider(
            "p",
            &["a.example.com"],
            Resolves::new()
                .with_needs_for(ResolveKind::Initial, [Input::ClientIp])
                .with_needs_for(ResolveKind::Refresh, [Input::HashedEmail]),
            ConsentPolicy::eid_persistence_only(),
        );
        let fixture = EligibilityFixture::owning(&provider, &["a.example.com"]);
        let initial = EligibilityView::empty();
        let mut refresh = EligibilityView::empty();
        refresh.set_record_for_test(
            &source("a.example.com"),
            record_for_test("example-a", None, 10),
        );
        let mut without_email = facts_for_writer();
        without_email.available_inputs.remove(&Input::HashedEmail);
        let mut without_address = facts_for_writer();
        without_address.available_inputs.remove(&Input::ClientIp);

        assert_eq!(
            fixture.skip_reason(&refresh, &without_email),
            SkipReason::NoSourceDue,
            "should leave out a Refresh whose needs are missing"
        );
        assert_eq!(
            fixture.skip_reason(&initial, &without_address),
            SkipReason::NoSourceDue,
            "should leave out an Initial whose needs are missing"
        );
        assert!(
            fixture.is_eligible(&initial, &without_email, Phase::AfterSend),
            "control: the Initial should not need the email"
        );
        assert!(
            fixture.is_eligible(&refresh, &facts_for_writer(), Phase::AfterSend),
            "control: should run when the inputs are available"
        );
    }

    #[test]
    fn future_n_and_sticky_opt_out_block() {
        let fixture = EligibilityFixture::new(&fixed_provider("p", &["a.example.com"]));
        let facts = facts_for_writer();

        let mut waiting = EligibilityView::empty();
        waiting.set_next_for_test("p", 1_001);
        assert_eq!(
            fixture.skip_reason(&waiting, &facts),
            SkipReason::NotYetEligible,
            "should wait for n"
        );

        let mut due = EligibilityView::empty();
        due.set_next_for_test("p", 1_000);
        assert!(
            fixture.is_eligible(&due, &facts, Phase::AfterSend),
            "control: should run once n has passed"
        );

        let mut opted_out = EligibilityView::empty();
        opted_out.set_opt_out_for_test("p", 500, None);
        assert_eq!(
            fixture.skip_reason(&opted_out, &facts),
            SkipReason::OptedOut,
            "should not call a provider the visitor opted out of"
        );
    }

    #[test]
    fn a_later_clear_lifts_the_opt_out() {
        let mut view = EligibilityView::empty();
        view.set_opt_out_for_test("p", 500, Some(600));
        assert!(
            !view.is_opted_out(&pid("p")),
            "should lift the opt-out when oc is later than o.at"
        );

        view.set_opt_out_for_test("p", 500, Some(500));
        assert!(
            view.is_opted_out(&pid("p")),
            "should keep the opt-out when oc is not strictly later"
        );

        view.set_opt_out_for_test("p", 500, Some(400));
        assert!(
            view.is_opted_out(&pid("p")),
            "should keep the opt-out when oc is older"
        );
        assert!(
            !view.is_opted_out(&pid("other")),
            "should not opt out a provider with no state"
        );
    }

    #[test]
    fn view_reads_only_the_kv_row() {
        let mut entry = KvEntry::new(&allowing_consent(), None, 10, "example.com");
        entry.ids.insert(
            "a.example.com".to_owned(),
            record_for_test("example-a", Some("p"), 900),
        );
        entry.ps.insert(
            "p".to_owned(),
            crate::ec::kv_types::KvProviderState {
                n: Some(2_000),
                f: 2,
                o: Some(KvOptOut {
                    at: 5,
                    fp: "1".to_owned(),
                }),
                oc: Some(4),
                iv: vec!["0123456789abcdef".to_owned()],
                ov: Vec::new(),
                st: None,
            },
        );

        let view = EligibilityView::from_kv(&entry);

        assert!(
            view.record(&source("a.example.com")).is_some(),
            "should hold the source's whole record"
        );
        assert_eq!(view.failures(&pid("p")), 2, "should hold f");
        assert_eq!(view.next_eligible(&pid("p")), Some(2_000), "should hold n");
        assert!(view.is_opted_out(&pid("p")), "should hold o and oc");
    }

    // Fingerprint

    #[test]
    fn fingerprint_bits_is_a_plain_hex_bitmask_and_changes_with_each_signal() {
        let policy = ConsentPolicy::strict()
            .with_tcf_purposes([2, 10])
            .with_tcf_vendor(5);
        let signals = PolicySignals::default();
        let gdpr = test_consent_gdpr_without_vendor(6);
        let bits = |consent: &ConsentContext, signals: PolicySignals| {
            let value = fingerprint_bits(&policy, consent, signals);
            assert!(
                value.len() <= 16 && value == value.to_lowercase(),
                "should be at most 16 lowercase hex digits"
            );
            u64::from_str_radix(&value, 16).expect("should parse as hex")
        };

        // Purposes 1, 2, 4 and 10 are read and granted: bits 0, 1, 3, 9.
        // Vendor 5 is granted: bit 10.
        let control = bits(&gdpr, signals);
        assert_eq!(
            control,
            (1 << 0) | (1 << 1) | (1 << 3) | (1 << 9) | (1 << 10),
            "should set one bit per granted purpose and the vendor bit"
        );

        let mut without_purpose_two = gdpr.clone();
        if let Some(tcf) = without_purpose_two.tcf.as_mut() {
            tcf.purpose_consents[1] = false;
        }
        assert_eq!(
            bits(&without_purpose_two, signals),
            control & !(1 << 1),
            "should clear only bit 1 when purpose 2 is not granted"
        );

        let without_vendor = test_consent_gdpr_without_vendor(5);
        assert_eq!(
            bits(&without_vendor, signals),
            control & !(1 << 10),
            "should clear only the vendor bit when the vendor is not granted"
        );

        let mut sale = gdpr.clone();
        sale.us_privacy = Some(decode_us_privacy("1YYN").expect("should decode us_privacy"));
        assert_eq!(
            bits(&sale, signals),
            control | (1 << 11),
            "should set the sale bit for a US Privacy sale opt-out"
        );

        let mut gpp_sale = gdpr.clone();
        gpp_sale.gpp = Some(GppConsent {
            version: 1,
            section_ids: vec![7],
            eu_tcf: None,
            us_sale_opt_out: Some(true),
            us_sharing_opt_out: None,
            us_targeted_advertising_opt_out: None,
            us_gpc: None,
            us_known_child: None,
            us_privacy_section: None,
            us_section_decode_failed: false,
        });
        assert_eq!(
            bits(&gpp_sale, signals),
            control | (1 << 11),
            "should set the sale bit for a GPP sale opt-out"
        );

        let mut gpc = gdpr.clone();
        gpc.gpc = true;
        assert_eq!(
            bits(&gpc, signals),
            control | (1 << 12),
            "should set the GPC bit"
        );

        assert_eq!(
            bits(&gdpr, PolicySignals { dnt: true }),
            control | (1 << 13),
            "should set the DNT bit"
        );
    }

    #[test]
    fn fingerprint_bits_leaves_a_relaxed_signal_at_zero_and_never_moves_a_bit() {
        let strict = ConsentPolicy::strict();
        let relaxed = ConsentPolicy::strict()
            .allowing_gpc()
            .allowing_dnt()
            .allowing_us_opt_out(UsOptOut::Sale);
        let mut consent = allowing_consent();
        consent.gpc = true;
        let signals = PolicySignals { dnt: true };

        assert_eq!(
            fingerprint_bits(&strict, &consent, signals),
            "3000",
            "should set the GPC and DNT bits at positions 12 and 13"
        );
        assert_eq!(
            fingerprint_bits(&relaxed, &consent, signals),
            "0",
            "should leave a relaxed signal at zero"
        );
        assert_eq!(
            fingerprint_bits(
                &ConsentPolicy::strict().with_tcf_purposes([7]),
                &consent,
                PolicySignals::default()
            ),
            "1000",
            "should keep GPC at bit 12 whatever purposes the policy reads"
        );
    }

    // Section 8 rows

    #[test]
    fn no_match_waits_retry_in_capped_at_seven_days() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let fixture = OutcomeFixture::new();
        let context = fixture.context_at(1_000);
        let default = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::no_match()),
            &EligibilityView::empty(),
            &context,
        );
        assert_eq!(
            default.state.n,
            Some(1_000 + 86_400),
            "should wait the 24 h default"
        );
        let long = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(
                Resolution::no_match().with_retry_in(Duration::from_secs(30 * 86_400)),
            ),
            &EligibilityView::empty(),
            &context,
        );
        assert_eq!(
            long.state.n,
            Some(1_000 + 7 * 86_400),
            "should cap the wait at 7 days"
        );
    }

    #[test]
    fn found_resets_failures_and_retry_in_applies_to_every_resolution() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let mut view = EligibilityView::empty();
        view.set_failures_for_test("p", 3);
        view.set_record_for_test(&a, record_for_test("example-old", Some("p"), 900));
        let fixture = OutcomeFixture::new();
        let context = fixture.context_at(1_000);

        let found = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(
                Resolution::found([ResolvedEid::new(&a, ResolvedUid::new("u"))])
                    .with_retry_in(Duration::from_secs(600)),
            ),
            &view,
            &context,
        );
        let invalidated = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(
                Resolution::invalidate(&a).with_retry_in(Duration::from_secs(600)),
            ),
            &view,
            &context,
        );

        assert_eq!(
            found.state.f,
            Some(0),
            "should reset the failure count on found"
        );
        assert_eq!(
            found.state.n,
            Some(1_000 + 600),
            "should wait at least retry_in after found"
        );
        assert_eq!(
            invalidated.changes.len(),
            1,
            "control: should remove the stored record"
        );
        assert_eq!(
            invalidated.state.n,
            Some(1_000 + 600),
            "should raise invalidate's n = now to retry_in"
        );
    }

    #[test]
    fn found_without_retry_in_sets_no_n() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let fixture = OutcomeFixture::new();

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::found([ResolvedEid::new(
                &a,
                ResolvedUid::new("u"),
            )])),
            &EligibilityView::empty(),
            &fixture.context_at(1_000),
        );

        assert_eq!(
            changes.state.n, None,
            "should leave the refresh schedule to the record's own timing"
        );
    }

    #[test]
    fn input_failure_backs_off_exponentially_to_a_day() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let fixture = OutcomeFixture::new();
        let context = fixture.context_at(1_000);
        let cases = [
            (0, 1, 3_600),
            (1, 2, 7_200),
            (4, 5, 57_600),
            (5, 6, 86_400),
            (u32::MAX, u32::MAX, 86_400),
        ];
        for (before, after, wait) in cases {
            let mut view = EligibilityView::empty();
            view.set_failures_for_test("p", before);
            let changes = outcome_to_changes(
                provider.as_ref(),
                CallOutcome::InputFailure,
                &view,
                &context,
            );
            assert_eq!(
                changes.state.f,
                Some(after),
                "should count the failure (f was {before})"
            );
            assert_eq!(
                changes.state.n,
                Some(1_000 + wait),
                "should back off min(1 h x 2^(f-1), 24 h) (f was {before})"
            );
        }
    }

    #[test]
    fn transport_failure_backs_off_to_an_hour() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let mut view = EligibilityView::empty();
        view.set_failures_for_test("p", 5);
        let fixture = OutcomeFixture::new();
        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::TransportFailure,
            &view,
            &fixture.context_at(1_000),
        );
        assert_eq!(changes.state.f, Some(6), "should count the failure");
        assert_eq!(
            changes.state.n,
            Some(1_000 + 3_600),
            "should back off min(5 min x 2^5, 1 h) = 1 h"
        );

        let first = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::TransportFailure,
            &EligibilityView::empty(),
            &fixture.context_at(1_000),
        );
        assert_eq!(
            first.state.n,
            Some(1_000 + 300),
            "should start at 5 minutes"
        );
    }

    #[test]
    fn hold_cut_off_waits_hold_retry_without_counting_a_failure() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let mut view = EligibilityView::empty();
        view.set_failures_for_test("p", 2);
        let fixture = OutcomeFixture::new();

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::HoldCutOff,
            &view,
            &fixture.context_at(1_000),
        );

        assert_eq!(
            changes.state.n,
            Some(1_000 + 300),
            "should wait hold_retry_sec"
        );
        assert_eq!(changes.state.f, None, "should not count a failure");
        assert!(changes.changes.is_empty(), "should change no stored ID");
    }

    #[test]
    fn skip_waits_refresh_after() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let fixture = OutcomeFixture::new().with_settings(ProviderSettings {
            refresh_after_sec: 777,
            ..ProviderSettings::default()
        });
        for outcome in [CallOutcome::Skipped, CallOutcome::InputUnavailable] {
            let changes = outcome_to_changes(
                provider.as_ref(),
                outcome,
                &EligibilityView::empty(),
                &fixture.context_at(1_000),
            );
            assert_eq!(
                changes.state.n,
                Some(1_000 + 777),
                "should wait refresh_after_sec"
            );
            assert_eq!(changes.state.f, None, "should not count a failure");
        }
    }

    #[test]
    fn burst_cap_waits_one_window_and_never_counts_a_failure() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let mut view = EligibilityView::empty();
        view.set_failures_for_test("p", 2);
        let fixture = OutcomeFixture::new();

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::BurstCapped,
            &view,
            &fixture.context_at(1_000),
        );

        assert_eq!(changes.state.n, Some(1_060), "should wait one 60 s window");
        assert_eq!(changes.state.f, None, "should never count a failure");
    }

    #[test]
    fn breaker_open_and_not_dispatched_change_nothing() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let fixture = OutcomeFixture::new();
        for outcome in [CallOutcome::BreakerOpen, CallOutcome::NotDispatched] {
            let changes = outcome_to_changes(
                provider.as_ref(),
                outcome,
                &EligibilityView::empty(),
                &fixture.context_at(1_000),
            );
            assert_eq!(
                changes,
                OutcomeChanges::default(),
                "should change nothing when the call was not made"
            );
        }
    }

    #[test]
    fn invalid_state_clears_state_and_keeps_f() {
        let provider = stateful_fixed_provider("p", &["a.example.com"]);
        let mut view = EligibilityView::empty();
        view.set_failures_for_test("p", 2);
        let fixture = OutcomeFixture::new().with_settings(ProviderSettings {
            refresh_after_sec: 777,
            ..ProviderSettings::default()
        });
        let context = fixture.context_at(1_000);

        let bare = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::InvalidState(None),
            &view,
            &context,
        );
        assert_eq!(bare.state.st, StateUpdate::Clear, "should clear st");
        assert_eq!(bare.state.f, None, "should keep f");
        assert_eq!(
            bare.state.n,
            Some(1_000 + 777),
            "should wait refresh_after_sec"
        );

        let with_result = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::InvalidState(Some(
                Resolution::no_match().with_retry_in(Duration::from_secs(60)),
            )),
            &view,
            &context,
        );
        let result = with_result
            .result
            .expect("should carry the result's own state change");
        assert_eq!(result.state.st, StateUpdate::Clear, "should clear st");
        assert_eq!(result.state.f, None, "should keep f");
        assert_eq!(
            result.state.n,
            Some(1_000 + 60),
            "should keep the n the result set"
        );
    }

    #[test]
    fn oversized_state_rejects_positive_part_but_keeps_removals() {
        let provider = stateful_fixed_provider("p", &["a.example.com", "b.example.com"]);
        let a = source("a.example.com");
        let b = source("b.example.com");
        let mut view = EligibilityView::empty();
        view.set_record_for_test(&b, record_for_test("example-old", Some("p"), 900));
        let resolution = Resolution::found([ResolvedEid::new(&a, ResolvedUid::new("example-a"))])
            .also_invalidate(&b)
            .replacing_state(&"x".repeat(2_000));

        let fixture = OutcomeFixture::new();
        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(resolution),
            &view,
            &fixture.context_at(1_000),
        );

        assert!(
            changes.result.is_none(),
            "should store no ID and no state change"
        );
        assert_eq!(
            changes.changes.len(),
            1,
            "should keep only the invalidation"
        );
        assert!(
            matches!(&changes.changes[0], PartnerIdChange::OwnerRemove { source, .. } if *source == b),
            "should still apply the invalidation"
        );
        assert_eq!(
            changes.state.st,
            StateUpdate::Keep,
            "should keep the old state"
        );
        assert_eq!(changes.state.f, Some(1), "should handle it as Malformed");
        assert_eq!(
            changes.state.n,
            Some(1_000 + 3_600),
            "should back off min(1 h x 2^0, 24 h)"
        );
    }

    #[test]
    fn opted_out_removes_what_it_wrote_and_clears_state() {
        let provider = fixed_provider("p", &["a.example.com", "b.example.com"]);
        let a = source("a.example.com");
        let b = source("b.example.com");
        let c = source("c.example.com");
        let mut view = EligibilityView::empty();
        view.set_record_for_test(&a, record_for_test("example-a", Some("p"), 900));
        view.set_record_for_test(&b, record_for_test("example-b", None, 900));
        view.set_record_for_test(&c, record_for_test("example-c", Some("p"), 900));
        let fixture = OutcomeFixture::new().with_fingerprint("5");

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::opted_out()),
            &view,
            &fixture.context_at(1_000),
        );

        assert_eq!(
            changes.changes,
            vec![
                PartnerIdChange::WriterRemove {
                    source: a,
                    writer: pid("p"),
                },
                PartnerIdChange::WriterRemove {
                    source: c,
                    writer: pid("p"),
                },
            ],
            "should remove every record the provider wrote, claimed or not, and no browser record"
        );
        assert_eq!(
            changes.state.opt_out,
            Some(KvOptOut {
                at: 1_000,
                fp: "5".to_owned()
            }),
            "should record the opt-out"
        );
        assert_eq!(changes.state.st, StateUpdate::Clear, "should clear st");
    }

    #[test]
    fn capture_only_opt_out_also_removes_browser_records_of_its_sources() {
        let provider = fixed_capture_provider("p", &["a.example.com", "b.example.com"]);
        let a = source("a.example.com");
        let b = source("b.example.com");
        let c = source("c.example.com");
        let mut view = EligibilityView::empty();
        view.set_record_for_test(&a, record_for_test("example-a", None, 900));
        view.set_record_for_test(&b, record_for_test("example-b", Some("q"), 900));
        view.set_record_for_test(&c, record_for_test("example-c", None, 900));
        let catalog = ProviderCatalog::new(vec![provider.clone()], BTreeMap::new());
        let fixture = OutcomeFixture::new().with_catalog(catalog);

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::opted_out()),
            &view,
            &fixture.context_at(1_000),
        );

        assert_eq!(
            changes.changes,
            vec![PartnerIdChange::WriterRemove {
                source: a,
                writer: pid("p"),
            }],
            "should remove the browser record of its own source only"
        );
    }

    #[test]
    fn opted_out_records_the_fingerprint_of_the_current_consent() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let policy = ConsentPolicy::strict();
        let mut consent = allowing_consent();
        consent.gpc = true;
        let fingerprint = fingerprint_bits(&policy, &consent, PolicySignals::default());
        let fixture = OutcomeFixture::new().with_fingerprint(&fingerprint);

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::opted_out()),
            &EligibilityView::empty(),
            &fixture.context_at(1_000),
        );

        assert_eq!(
            changes.state.opt_out,
            Some(KvOptOut {
                at: 1_000,
                fp: "1000".to_owned()
            }),
            "should store the fingerprint of the consent in force"
        );
    }

    #[test]
    fn stateful_opt_out_goes_into_the_result_with_its_removals_outside() {
        let provider = stateful_fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let mut view = EligibilityView::empty();
        view.set_record_for_test(&a, record_for_test("example-a", Some("p"), 900));
        let fixture = OutcomeFixture::new();

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::opted_out()),
            &view,
            &fixture.context_at(1_000),
        );

        assert_eq!(changes.changes.len(), 1, "should list the removal");
        let result = changes
            .result
            .expect("should carry the opt-out in one result");
        assert!(result.ids.is_empty(), "should hold no positive change");
        assert!(result.state.opt_out.is_some(), "should record the opt-out");
        assert_eq!(result.state.st, StateUpdate::Clear, "should clear st");
    }

    #[test]
    fn invalidate_removes_the_set_and_sets_n_now() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let record = record_for_test("example-a", Some("p"), 900);
        let expected = record_revision(&record);
        let mut view = EligibilityView::empty();
        view.set_record_for_test(&a, record);
        let fixture = OutcomeFixture::new();

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::invalidate(&a)),
            &view,
            &fixture.context_at(1_000),
        );

        assert_eq!(
            changes.changes,
            vec![PartnerIdChange::OwnerRemove {
                source: a,
                provider: pid("p"),
                expected,
            }],
            "should remove the set at the revision read"
        );
        assert_eq!(changes.state.n, Some(1_000), "should set n = now");
        assert_eq!(changes.state.f, None, "should not touch f");
    }

    #[test]
    fn invalidating_a_source_without_a_record_makes_no_change() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let fixture = OutcomeFixture::new();

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::invalidate(&a)),
            &EligibilityView::empty(),
            &fixture.context_at(1_000),
        );

        assert_eq!(
            changes,
            OutcomeChanges::default(),
            "should change nothing for a source with no record"
        );
    }

    #[test]
    fn invalidating_an_unclaimed_source_makes_no_change() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let c = source("c.example.com");
        let mut view = EligibilityView::empty();
        view.set_record_for_test(&c, record_for_test("example-c", Some("q"), 900));
        let fixture = OutcomeFixture::new();

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::invalidate(&c)),
            &view,
            &fixture.context_at(1_000),
        );

        assert!(
            changes.changes.is_empty(),
            "should not let a provider remove a source it does not claim"
        );
    }

    // Entry sets

    #[test]
    fn found_with_several_entries_becomes_one_whole_set_per_source() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let resolution = Resolution::found([
            ResolvedEid::new(&a, ResolvedUid::new("example-a").with_atype(1))
                .with_uid(
                    ResolvedUid::new("example-b")
                        .with_atype(3)
                        .with_ext(json!({ "stype": "ppuid" })),
                )
                .with_inserter("example.com")
                .with_mm(3),
            ResolvedEid::new(&a, ResolvedUid::new("example-c")).with_matcher("partner.example.com"),
        ]);

        let fixture = OutcomeFixture::new();
        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(resolution),
            &EligibilityView::empty(),
            &fixture.context_at(1_000),
        );

        assert_eq!(
            changes.changes.len(),
            1,
            "should make one change for the one source"
        );
        let record = positive_record(&changes.changes[0]).expect("should be a positive change");
        assert_eq!(record.w.as_deref(), Some("p"), "should name the writer");
        assert_eq!(record.at, Some(1_000), "should stamp the write time");
        assert_eq!(
            record.eids,
            vec![
                KvEid {
                    uids: vec![
                        KvUid {
                            id: "example-a".to_owned(),
                            atype: Some(1),
                            ext: None
                        },
                        KvUid {
                            id: "example-b".to_owned(),
                            atype: Some(3),
                            ext: Some(json!({ "stype": "ppuid" }))
                        },
                    ],
                    inserter: Some("example.com".to_owned()),
                    matcher: None,
                    mm: Some(3),
                    ext: None,
                },
                KvEid {
                    uids: vec![KvUid {
                        id: "example-c".to_owned(),
                        atype: None,
                        ext: None
                    }],
                    inserter: None,
                    matcher: Some("partner.example.com".to_owned()),
                    mm: None,
                    ext: None,
                },
            ],
            "should store the whole set, in the order the provider returned it"
        );
    }

    #[test]
    fn one_uid_result_without_eid_fields_becomes_a_one_entry_record() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let fixture = OutcomeFixture::new();

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::found([ResolvedEid::new(
                &a,
                ResolvedUid::new("example-a"),
            )])),
            &EligibilityView::empty(),
            &fixture.context_at(1_000),
        );

        let record = positive_record(&changes.changes[0]).expect("should be a positive change");
        assert_eq!(
            record.eids,
            KvPartnerId::new("example-a").eids,
            "should write the shape browser ingest writes"
        );
        assert_eq!(record.rv, None, "should leave the revision to the merge");
    }

    #[test]
    fn owned_source_becomes_owner_write_and_unowned_becomes_fill_missing() {
        let provider = fixed_provider("p", &["a.example.com", "b.example.com"]);
        let a = source("a.example.com");
        let b = source("b.example.com");
        let catalog = ProviderCatalog::new(
            vec![provider.clone()],
            BTreeMap::from([(a.clone(), pid("p"))]),
        );
        let stored = record_for_test("example-old", Some("p"), 900);
        let expected = record_revision(&stored);
        let mut view = EligibilityView::empty();
        view.set_record_for_test(&a, stored);
        let fixture = OutcomeFixture::new().with_catalog(catalog);

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::found([
                ResolvedEid::new(&a, ResolvedUid::new("example-a")),
                ResolvedEid::new(&b, ResolvedUid::new("example-b")),
            ])),
            &view,
            &fixture.context_at(1_000),
        );

        assert!(
            changes.changes.iter().any(|change| matches!(
                change,
                PartnerIdChange::OwnerWrite { source, expected: Some(revision), .. }
                    if *source == a && *revision == expected
            )),
            "should write the owned source at the revision read"
        );
        assert!(
            changes.changes.iter().any(|change| matches!(
                change,
                PartnerIdChange::FillMissing { source, .. } if *source == b
            )),
            "should only fill the unowned source"
        );

        let fresh = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::found([ResolvedEid::new(
                &a,
                ResolvedUid::new("example-a"),
            )])),
            &EligibilityView::empty(),
            &fixture.context_at(1_000),
        );
        assert!(
            matches!(
                &fresh.changes[0],
                PartnerIdChange::OwnerWrite { expected: None, .. }
            ),
            "should expect no record when the owned source had none"
        );
    }

    #[test]
    fn stateful_found_goes_into_one_provider_result_with_expected_state() {
        let provider = stateful_fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let expected_state = StateRevision::of(Some(&json!({ "token": "example-old" })));
        let fixture = OutcomeFixture::new().with_expected_state(expected_state.clone());

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(
                Resolution::found([ResolvedEid::new(&a, ResolvedUid::new("example-a"))])
                    .replacing_state(&json!({ "token": "example-new" })),
            ),
            &EligibilityView::empty(),
            &fixture.context_at(1_000),
        );

        assert!(
            changes.changes.is_empty(),
            "should hold no stand-alone change"
        );
        assert_eq!(
            changes.state,
            ProviderStateChange::default(),
            "should hold no stand-alone state"
        );
        let result = changes.result.expect("should produce one result");
        assert_eq!(result.provider, pid("p"), "should name the provider");
        assert_eq!(result.ids.len(), 1, "should carry the positive change");
        assert_eq!(
            result.state.st,
            StateUpdate::Replace(json!({ "token": "example-new" })),
            "should replace st"
        );
        assert_eq!(result.state.f, Some(0), "should reset f in the result");
        assert_eq!(
            result.expected_state,
            Some(expected_state),
            "should expect the state revision the call read"
        );
    }

    #[test]
    fn unclaimed_source_entries_are_discarded() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let c = source("c.example.com");
        let fixture = OutcomeFixture::new();

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::found([
                ResolvedEid::new(&a, ResolvedUid::new("example-a")),
                ResolvedEid::new(&c, ResolvedUid::new("example-c")),
            ])),
            &EligibilityView::empty(),
            &fixture.context_at(1_000),
        );

        assert_eq!(
            changes.changes.len(),
            1,
            "should keep only the claimed source"
        );
        assert!(
            matches!(&changes.changes[0], PartnerIdChange::FillMissing { source, .. } if *source == a),
            "should drop the source the provider does not claim"
        );
    }

    #[test]
    fn found_naming_only_unclaimed_sources_is_malformed() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let c = source("c.example.com");
        let mut view = EligibilityView::empty();
        view.set_failures_for_test("p", 3);
        let fixture = OutcomeFixture::new();

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::found([ResolvedEid::new(
                &c,
                ResolvedUid::new("example-c"),
            )])),
            &view,
            &fixture.context_at(1_000),
        );

        assert!(changes.changes.is_empty(), "should store nothing");
        assert_eq!(changes.state.f, Some(4), "should count a failure");
        assert_eq!(
            changes.state.n,
            Some(1_000 + 28_800),
            "should back off min(1 h x 2^3, 24 h)"
        );
    }

    #[test]
    fn one_claimed_and_one_unclaimed_source_still_resets_failures() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let c = source("c.example.com");
        let mut view = EligibilityView::empty();
        view.set_failures_for_test("p", 3);
        let fixture = OutcomeFixture::new();

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::found([
                ResolvedEid::new(&a, ResolvedUid::new("example-a")),
                ResolvedEid::new(&c, ResolvedUid::new("example-c")),
            ])),
            &view,
            &fixture.context_at(1_000),
        );

        assert_eq!(changes.changes.len(), 1, "should store the claimed source");
        assert_eq!(changes.state.f, Some(0), "should reset f on found");
    }

    #[test]
    fn oversized_set_from_a_stateless_provider_leaves_only_that_source_unchanged() {
        let provider = fixed_provider("p", &["a.example.com", "b.example.com"]);
        let a = source("a.example.com");
        let b = source("b.example.com");
        let mut entries = large_entries(&a);
        entries.push(ResolvedEid::new(&b, ResolvedUid::new("example-b")));
        let fixture = OutcomeFixture::new();

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::found(entries)),
            &EligibilityView::empty(),
            &fixture.context_at(1_000),
        );

        assert_eq!(changes.changes.len(), 1, "should keep the other source");
        assert!(
            matches!(&changes.changes[0], PartnerIdChange::FillMissing { source, .. } if *source == b),
            "should leave the oversized source as it is"
        );
        assert_eq!(changes.state.f, Some(0), "should still count as found");
    }

    #[test]
    fn oversized_set_from_a_stateful_provider_rejects_the_positive_part_and_keeps_removals() {
        let provider = stateful_fixed_provider("p", &["a.example.com", "b.example.com"]);
        let a = source("a.example.com");
        let b = source("b.example.com");
        let mut view = EligibilityView::empty();
        view.set_record_for_test(&b, record_for_test("example-old", Some("p"), 900));
        let resolution = Resolution::found(large_entries(&a))
            .also_invalidate(&b)
            .replacing_state(&json!({ "token": "example-token" }));

        let fixture = OutcomeFixture::new();
        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(resolution),
            &view,
            &fixture.context_at(1_000),
        );

        assert!(
            changes.result.is_none(),
            "should store no ID and no state change"
        );
        assert!(
            changes.changes.iter().any(
                |change| matches!(change, PartnerIdChange::OwnerRemove { source, .. } if *source == b)
            ),
            "should still apply the invalidation"
        );
        assert!(
            changes
                .changes
                .iter()
                .all(|change| positive_record(change).is_none()),
            "should store no positive change"
        );
        assert!(
            matches!(changes.state.st, StateUpdate::Keep),
            "should keep the old state"
        );
        assert_eq!(changes.state.f, Some(1), "should handle it as Malformed");
        assert_eq!(
            changes.state.n,
            Some(1_000 + 3_600),
            "should back off min(1 h x 2^0, 24 h)"
        );
    }

    #[test]
    fn result_whose_every_set_is_discarded_is_malformed() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let mut view = EligibilityView::empty();
        view.set_failures_for_test("p", 1);
        view.set_record_for_test(
            &source("b.example.com"),
            record_for_test("example-b", Some("p"), 900),
        );
        let fixture = OutcomeFixture::new();

        let oversized = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(
                Resolution::found(large_entries(&a)).also_invalidate(&source("b.example.com")),
            ),
            &view,
            &fixture.context_at(1_000),
        );
        let dropped = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::found([ResolvedEid::new(
                &a,
                ResolvedUid::new("  "),
            )])),
            &view,
            &fixture.context_at(1_000),
        );

        for (name, changes) in [("oversized", &oversized), ("dropped", &dropped)] {
            assert_eq!(changes.state.f, Some(2), "should count a failure ({name})");
            assert_eq!(
                changes.state.n,
                Some(1_000 + 7_200),
                "should back off min(1 h x 2^1, 24 h) ({name})"
            );
            assert!(
                changes
                    .changes
                    .iter()
                    .all(|change| positive_record(change).is_none()),
                "should store no positive change ({name})"
            );
        }
        assert_eq!(
            oversized.changes.len(),
            1,
            "should still apply the removal of an oversized result"
        );
    }

    #[test]
    fn invalidating_a_set_carries_no_hash_until_it_applies() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let fixture = OutcomeFixture::new();
        let mut record = record_for_test("example-0-0", Some("p"), 900);
        record.eids = (0..4)
            .map(|chunk| KvEid {
                uids: (0..8)
                    .map(|index| KvUid {
                        id: format!("example-{chunk}-{index}"),
                        atype: None,
                        ext: None,
                    })
                    .collect(),
                inserter: None,
                matcher: None,
                mm: None,
                ext: None,
            })
            .collect();
        let expected = record_revision(&record);
        let mut view = EligibilityView::empty();
        view.set_record_for_test(&a, record);

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::invalidate(&a)),
            &view,
            &fixture.context_at(1_000),
        );

        assert_eq!(
            changes.changes.len(),
            1,
            "should make one removal for the 32-UID set"
        );
        assert!(
            matches!(
                &changes.changes[0],
                PartnerIdChange::OwnerRemove { source, provider, expected: revision }
                    if *source == a && provider.as_str() == "p" && *revision == expected
            ),
            "should remove the whole set at the revision it read"
        );
        assert_eq!(changes.state.n, Some(1_000), "should set n = now");
    }

    #[test]
    fn expiring_set_without_expiry_keeps_stored_exp_only_when_unchanged() {
        let provider = expiring_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let catalog = ProviderCatalog::new(
            vec![provider.clone()],
            BTreeMap::from([(a.clone(), pid("p"))]),
        );
        let fixture = OutcomeFixture::new().with_catalog(catalog);
        let mut stored = record_for_test("example-a", Some("p"), 900);
        stored.exp = Some(5_000);
        let mut view = EligibilityView::empty();
        view.set_record_for_test(&a, stored);

        let unchanged = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::found([ResolvedEid::new(
                &a,
                ResolvedUid::new("example-a"),
            )])),
            &view,
            &fixture.context_at(1_000),
        );
        let changed = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::found([ResolvedEid::new(
                &a,
                ResolvedUid::new("example-other"),
            )])),
            &view,
            &fixture.context_at(1_000),
        );
        let with_expiry = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::found([ResolvedEid::new(
                &a,
                ResolvedUid::new("example-other"),
            )
            .with_expires_in(Duration::from_secs(60))])),
            &view,
            &fixture.context_at(1_000),
        );

        let kept = positive_record(&unchanged.changes[0]).expect("should store the unchanged set");
        assert_eq!(kept.exp, Some(5_000), "should keep the stored exp");
        assert_eq!(unchanged.state.f, Some(0), "should count as found");

        assert!(
            changed.changes.is_empty(),
            "should discard a changed set that carries no expiry"
        );
        assert_eq!(
            changed.state.f,
            Some(1),
            "should handle a result whose every set is discarded as Malformed"
        );

        let fresh = positive_record(&with_expiry.changes[0]).expect("should store the set");
        assert_eq!(fresh.exp, Some(1_060), "should use the result's own expiry");
    }

    #[test]
    fn result_without_a_source_leaves_its_set_alone() {
        let provider = fixed_provider("p", &["a.example.com", "b.example.com"]);
        let a = source("a.example.com");
        let b = source("b.example.com");
        let mut view = EligibilityView::empty();
        view.set_record_for_test(&b, record_for_test("example-b", Some("p"), 900));
        let fixture = OutcomeFixture::new();

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::found([ResolvedEid::new(
                &a,
                ResolvedUid::new("example-a"),
            )])),
            &view,
            &fixture.context_at(1_000),
        );

        assert_eq!(
            changes.changes.len(),
            1,
            "should change only the named source"
        );
        assert!(
            matches!(&changes.changes[0], PartnerIdChange::FillMissing { source, .. } if *source == a),
            "should leave the source the result did not name"
        );
    }

    #[test]
    fn record_carries_the_earliest_times_of_its_set() {
        let provider = stateful_fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let fixture = OutcomeFixture::new();
        let resolution = Resolution::found([
            ResolvedEid::new(&a, ResolvedUid::new("example-a"))
                .with_expires_in(Duration::from_secs(600))
                .with_refresh_in(Duration::from_secs(300))
                .with_refresh_until(UnixTime::from_secs(9_000)),
            ResolvedEid::new(&a, ResolvedUid::new("example-b"))
                .with_expires_in(Duration::from_secs(60))
                .with_refresh_in(Duration::from_secs(900))
                .with_refresh_until(UnixTime::from_secs(8_000)),
        ]);

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(resolution),
            &EligibilityView::empty(),
            &fixture.context_at(1_000),
        );

        let result = changes.result.expect("should produce one result");
        let record = positive_record(&result.ids[0]).expect("should be a positive change");
        assert_eq!(record.exp, Some(1_060), "should keep the earliest expiry");
        assert_eq!(record.ra, Some(1_300), "should keep the earliest refresh");
        assert_eq!(
            record.ru,
            Some(8_000),
            "should keep the earliest refresh limit"
        );
    }

    #[test]
    fn refresh_until_is_ignored_for_a_stateless_provider() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let fixture = OutcomeFixture::new();

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::found([ResolvedEid::new(
                &a,
                ResolvedUid::new("example-a"),
            )
            .with_refresh_until(UnixTime::from_secs(9_000))])),
            &EligibilityView::empty(),
            &fixture.context_at(1_000),
        );

        let record = positive_record(&changes.changes[0]).expect("should be a positive change");
        assert_eq!(
            record.ru, None,
            "should ignore refresh_until without private state"
        );
    }

    #[test]
    fn duplicate_entries_collapse_into_one_entry() {
        // A duplicate entry is dropped, so the kept set has one entry.
        let provider = fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let fixture = OutcomeFixture::new();

        let changes = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::found([
                ResolvedEid::new(&a, ResolvedUid::new("example-a")),
                ResolvedEid::new(&a, ResolvedUid::new("example-a")),
            ])),
            &EligibilityView::empty(),
            &fixture.context_at(1_000),
        );

        let record = positive_record(&changes.changes[0]).expect("should be a positive change");
        assert_eq!(
            record.eids,
            vec![one_entry("example-a")],
            "should drop the duplicate"
        );
    }

    // Dispatch

    struct RecordingLimiter {
        exceeded: bool,
        fail: bool,
        keys: std::sync::Mutex<Vec<String>>,
    }

    impl RecordingLimiter {
        fn new(exceeded: bool, fail: bool) -> Self {
            Self {
                exceeded,
                fail,
                keys: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl RateLimiter for RecordingLimiter {
        fn exceeded(
            &self,
            key: &str,
            _hourly_limit: u32,
        ) -> Result<bool, Report<crate::error::TrustedServerError>> {
            self.keys
                .lock()
                .expect("should lock the keys")
                .push(key.to_owned());
            if self.fail {
                return Err(Report::new(crate::error::TrustedServerError::Proxy {
                    message: "limiter unavailable".to_owned(),
                }));
            }
            Ok(self.exceeded)
        }
    }

    /// Providers, a fake client and everything `dispatch` reads.
    struct DispatchFixture {
        settings: Settings,
        catalog: ProviderCatalog,
        keys: IdentityKeys,
        client: Arc<FakeHttpClient>,
        backend: Arc<FakeBackend>,
        services: RuntimeServices,
    }

    impl DispatchFixture {
        fn new(providers: Vec<Arc<dyn IdentityProvider>>) -> Self {
            let settings = create_test_settings();
            let client = Arc::new(FakeHttpClient::new());
            let backend = Arc::new(FakeBackend::new());
            let services = fake_services(&client, &backend);
            Self {
                keys: IdentityKeys::from_settings(&settings),
                catalog: ProviderCatalog::new(providers, BTreeMap::new()),
                settings,
                client,
                backend,
                services,
            }
        }

        fn run(
            &self,
            ids: &[&str],
            now: u64,
            budget: Option<Duration>,
            limiter: Option<&dyn RateLimiter>,
        ) -> Vec<(IdentityProviderId, CallOutcome)> {
            self.run_with_state(ids, &[], now, budget, limiter)
        }

        fn run_with_state(
            &self,
            ids: &[&str],
            states: &[(&str, &str)],
            now: u64,
            budget: Option<Duration>,
            limiter: Option<&dyn RateLimiter>,
        ) -> Vec<(IdentityProviderId, CallOutcome)> {
            let deps = IdentityDeps {
                settings: &self.settings,
                catalog: &self.catalog,
                keys: &self.keys,
                random: &SeededRandom::new(1),
                services: &self.services,
                rate_limiter: limiter,
            };
            let calls = ids
                .iter()
                .map(|id| {
                    let provider = self
                        .catalog
                        .get(&pid(id))
                        .expect("should hold the provider");
                    Call {
                        state_json: states
                            .iter()
                            .find(|(owner, _)| owner == id)
                            .map(|(_, state)| (*state).to_owned()),
                        provider: pid(id),
                        kinds: BTreeMap::from([(
                            provider.descriptor().primary_source().clone(),
                            ResolveKind::Initial,
                        )]),
                    }
                })
                .collect();
            let signals = ResolveSignals {
                client_ip: None,
                user_agent: Some("ExampleBrowser/1.0"),
                page_url: None,
                region: None,
                hashed_email: None,
                consent: ProviderConsent::default(),
                publisher_domain: "www.example.com",
                now: UnixTime::from_secs(now),
                random: &ThreadRandom,
                ec_id: Some(format!("{}.ABC123", "a".repeat(64))),
                stored: BTreeMap::new(),
            };
            dispatch(&deps, calls, &signals, budget)
        }
    }

    fn outcome_of<'a>(
        outcomes: &'a [(IdentityProviderId, CallOutcome)],
        id: &str,
    ) -> &'a CallOutcome {
        &outcomes
            .iter()
            .find(|(provider, _)| *provider == pid(id))
            .expect("should hold an outcome for the provider")
            .1
    }

    fn scripted(id: &str, host: &str, source: &str) -> Arc<dyn IdentityProvider> {
        ScriptedProvider::new(id, host, source, false).into_provider()
    }

    #[test]
    fn check_request_enforces_https_host_and_no_cookie() {
        let descriptor = fixed_provider("p", &["a.example.com"]).descriptor().clone();
        let good = ProviderRequest::get(
            &"https://api.example.com/x"
                .parse()
                .expect("should parse the URL"),
        );
        check_request(&good, &descriptor)
            .expect("should allow an https request to a declared host (control)");
        for (name, request) in [
            (
                "http",
                ProviderRequest::get(
                    &"http://api.example.com/x"
                        .parse()
                        .expect("should parse the URL"),
                ),
            ),
            (
                "host",
                ProviderRequest::get(
                    &"https://other.example.net/x"
                        .parse()
                        .expect("should parse the URL"),
                ),
            ),
            (
                "cookie",
                good.clone()
                    .try_with_header("cookie", "a=1")
                    .expect("should add the header"),
            ),
        ] {
            assert!(
                check_request(&request, &descriptor).is_err(),
                "should reject the {name} case"
            );
        }
    }

    #[test]
    fn decode_body_caps_decoded_size() {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(&vec![b'a'; 10_000])
            .expect("should write to the encoder");
        let gzipped = encoder.finish().expect("should finish the gzip stream");
        assert_eq!(
            decode_body(Some("gzip"), gzipped.clone(), 10_000).map(|body| body.len()),
            Some(10_000),
            "should decode a body that fits"
        );
        assert!(
            decode_body(Some("gzip"), gzipped, 9_999).is_none(),
            "should reject a body over the cap after decoding"
        );
        assert!(
            decode_body(Some("zstd"), vec![1], 10).is_none(),
            "should reject an unknown encoding"
        );
        assert_eq!(
            decode_body(None, b"ok".to_vec(), 10).as_deref(),
            Some(&b"ok"[..]),
            "should pass an identity body through"
        );
        assert!(
            decode_body(None, vec![0; 11], 10).is_none(),
            "should reject an identity body over the cap"
        );
    }

    #[test]
    fn dispatch_maps_status_codes_to_outcomes() {
        let mut fixture = DispatchFixture::new(vec![
            scripted("ok", "ok.example.com", "ok.example.com"),
            scripted("busy", "busy.example.com", "busy.example.com"),
            scripted("bad", "bad.example.com", "bad.example.com"),
            scripted("big", "big.example.com", "big.example.com"),
            scripted("long", "long.example.com", "long.example.com"),
        ]);
        fixture.settings.ec.identity.max_response_bytes = 8;
        fixture.client.respond("ok.example.com", 200, b"{}");
        fixture.client.respond("busy.example.com", 503, b"{}");
        fixture.client.respond("bad.example.com", 400, b"{}");
        fixture
            .client
            .respond("big.example.com", 200, b"0123456789");
        fixture.client.respond_with_headers(
            "long.example.com",
            200,
            b"{}",
            &[("content-length", "9999")],
        );

        let outcomes = fixture.run(&["ok", "busy", "bad", "big", "long"], 1_000, None, None);

        assert!(
            matches!(outcome_of(&outcomes, "ok"), CallOutcome::Resolved(_)),
            "should resolve a 200"
        );
        assert!(
            matches!(outcome_of(&outcomes, "busy"), CallOutcome::TransportFailure),
            "should count a 503 as a transport failure"
        );
        assert!(
            matches!(outcome_of(&outcomes, "bad"), CallOutcome::InputFailure),
            "should count a 400 as an input failure"
        );
        assert!(
            matches!(outcome_of(&outcomes, "big"), CallOutcome::TransportFailure),
            "should count a body over the cap as a transport failure"
        );
        assert!(
            matches!(outcome_of(&outcomes, "long"), CallOutcome::TransportFailure),
            "should count a Content-Length over the cap as a transport failure"
        );
    }

    #[test]
    fn dispatch_rejects_a_body_that_decodes_over_the_cap() {
        let mut fixture =
            DispatchFixture::new(vec![scripted("p", "p.example.com", "p.example.com")]);
        fixture.settings.ec.identity.max_response_bytes = 4_096;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(&vec![b'a'; 100_000])
            .expect("should write to the encoder");
        let gzipped = encoder.finish().expect("should finish the gzip stream");
        assert!(
            gzipped.len() < 4_096,
            "should compress under the cap (control)"
        );
        fixture.client.respond_with_headers(
            "p.example.com",
            200,
            &gzipped,
            &[("content-encoding", "gzip")],
        );

        let outcomes = fixture.run(&["p"], 1_000, None, None);

        assert!(
            matches!(outcome_of(&outcomes, "p"), CallOutcome::TransportFailure),
            "should count a decompression bomb as a transport failure"
        );
    }

    #[test]
    fn after_send_timeout_is_a_transport_failure_not_a_cut_off() {
        let fixture = DispatchFixture::new(vec![scripted("p", "p.example.com", "p.example.com")]);
        fixture.client.time_out("p.example.com");

        let outcomes = fixture.run(&["p"], 1_000, None, None);

        assert!(
            matches!(outcome_of(&outcomes, "p"), CallOutcome::TransportFailure),
            "should count a missed after-send timeout as a transport failure"
        );
        assert_eq!(
            fixture.backend.first_byte_timeouts(),
            vec![Duration::from_millis(
                ProviderSettings::default_after_send_timeout_ms()
            )],
            "should wait the after-send timeout for the first byte"
        );
    }

    #[test]
    fn hold_dispatch_cuts_off_a_call_the_budget_ran_out_on() {
        let fixture = DispatchFixture::new(vec![
            scripted("a", "a.example.com", "a.example.com"),
            scripted("b", "b.example.com", "b.example.com"),
            scripted("c", "c.example.com", "c.example.com"),
        ]);
        fixture.client.respond("a.example.com", 200, b"{}");
        fixture.client.respond("b.example.com", 200, b"{}");
        fixture.client.respond("c.example.com", 200, b"{}");
        fixture
            .client
            .delay("a.example.com", Duration::from_millis(60));
        fixture.client.time_out("c.example.com");

        let outcomes = fixture.run(
            &["a", "b", "c"],
            1_000,
            Some(Duration::from_millis(20)),
            None,
        );

        assert!(
            matches!(outcome_of(&outcomes, "a"), CallOutcome::Resolved(_)),
            "should keep a response that arrived in flight"
        );
        assert!(
            matches!(outcome_of(&outcomes, "b"), CallOutcome::HoldCutOff),
            "should drop a call still pending when the budget ran out"
        );
        assert!(
            matches!(outcome_of(&outcomes, "c"), CallOutcome::HoldCutOff),
            "should drop a call the budget ran out before waiting on"
        );
        assert_eq!(
            fixture.backend.first_byte_timeouts().len(),
            3,
            "should have sent all three before the budget ran out"
        );
    }

    #[test]
    fn hold_failure_before_the_budget_is_spent_is_a_transport_failure() {
        let mut fixture =
            DispatchFixture::new(vec![scripted("p", "p.example.com", "p.example.com")]);
        fixture.settings.ec.identity.providers.insert(
            "p".to_owned(),
            ProviderSettings {
                timeout_ms: Some(10),
                ..ProviderSettings::default()
            },
        );
        fixture.client.time_out("p.example.com");

        let outcomes = fixture.run(&["p"], 1_000, Some(Duration::from_secs(60)), None);

        assert!(
            matches!(outcome_of(&outcomes, "p"), CallOutcome::TransportFailure),
            "should count a failure under timeout_ms as a transport failure, not a cut off"
        );
    }

    #[test]
    fn each_call_reads_only_its_own_providers_state() {
        let fixture = DispatchFixture::new(vec![
            ScriptedProvider::new("one", "one.example.com", "one.example.com", false)
                .stateful()
                .into_provider(),
            ScriptedProvider::new("two", "two.example.com", "two.example.com", false)
                .stateful()
                .into_provider(),
            ScriptedProvider::new("three", "three.example.com", "three.example.com", false)
                .stateful()
                .into_provider(),
        ]);
        for host in ["one", "two", "three"] {
            fixture
                .client
                .respond(&format!("{host}.example.com"), 200, b"{}");
        }

        fixture.run_with_state(
            &["one", "two", "three"],
            &[
                ("one", r#"{"token":"first-secret"}"#),
                ("two", r#"{"token":"second-secret"}"#),
            ],
            1_000,
            None,
            None,
        );

        let uris = fixture.client.sent_uris();
        let uri_for = |host: &str| {
            uris.iter()
                .find(|uri| uri.contains(host))
                .expect("should have sent to the host")
                .clone()
        };
        let first = uri_for("one.example.com");
        assert!(
            first.contains("first-secret") && !first.contains("second-secret"),
            "should give the first provider only its own state"
        );
        let second = uri_for("two.example.com");
        assert!(
            second.contains("second-secret") && !second.contains("first-secret"),
            "should give the second provider only its own state"
        );
        assert!(
            uri_for("three.example.com").contains("state=none"),
            "should give a provider with no stored state none"
        );
    }

    #[test]
    fn hold_dispatch_with_no_budget_sends_nothing() {
        let fixture = DispatchFixture::new(vec![scripted("p", "p.example.com", "p.example.com")]);
        fixture.client.respond("p.example.com", 200, b"{}");

        let outcomes = fixture.run(&["p"], 1_000, Some(Duration::ZERO), None);

        assert!(
            matches!(outcome_of(&outcomes, "p"), CallOutcome::HoldCutOff),
            "should cut off a hold call with no budget left"
        );
        assert!(
            fixture.client.sent_hosts().is_empty(),
            "should send nothing"
        );
    }

    #[test]
    fn hold_dispatch_waits_the_smaller_of_the_hold_timeout_and_the_budget() {
        let mut fixture =
            DispatchFixture::new(vec![scripted("p", "p.example.com", "p.example.com")]);
        fixture.settings.ec.identity.hold_budget_ms = 80;
        fixture.client.respond("p.example.com", 200, b"{}");

        fixture.run(&["p"], 1_000, Some(Duration::from_secs(10)), None);
        fixture.run(&["p"], 1_000, Some(Duration::from_millis(30)), None);

        let timeouts = fixture.backend.first_byte_timeouts();
        assert_eq!(
            timeouts[0],
            Duration::from_millis(80),
            "should wait the hold timeout when the budget is larger"
        );
        assert!(
            timeouts[1] <= Duration::from_millis(30),
            "should wait no longer than the remaining budget"
        );
    }

    #[test]
    fn burst_cap_skips_without_counting_a_failure() {
        let fixture = DispatchFixture::new(vec![scripted("p", "p.example.com", "p.example.com")]);
        fixture.client.respond("p.example.com", 200, b"{}");
        let capped = RecordingLimiter::new(true, false);
        let broken = RecordingLimiter::new(false, true);

        let outcomes = fixture.run(&["p"], 1_000, None, Some(&capped));
        assert!(
            matches!(outcome_of(&outcomes, "p"), CallOutcome::BurstCapped),
            "should report a burst-capped call"
        );
        assert!(
            fixture.client.sent_hosts().is_empty(),
            "should not send a burst-capped call"
        );
        let keys = capped.keys.lock().expect("should lock the keys");
        assert!(
            keys[0].starts_with("identity:p:") && !keys[0].contains(&"a".repeat(64)),
            "should key the cap by provider and the keyed visitor hash, not the EC ID"
        );
        drop(keys);

        let outcomes = fixture.run(&["p"], 1_000, None, Some(&broken));
        assert!(
            matches!(outcome_of(&outcomes, "p"), CallOutcome::NotDispatched),
            "should not dispatch when the limiter cannot be read"
        );
        assert!(
            fixture.client.sent_hosts().is_empty(),
            "should not send when the limiter cannot be read"
        );

        let allowing = RecordingLimiter::new(false, false);
        let outcomes = fixture.run(&["p"], 1_000, None, Some(&allowing));
        assert!(
            matches!(outcome_of(&outcomes, "p"), CallOutcome::Resolved(_)),
            "should dispatch under the cap (control)"
        );
    }

    #[test]
    fn undeclared_read_is_an_input_failure_even_when_ignored() {
        let fixture = DispatchFixture::new(vec![
            ScriptedProvider::new("p", "p.example.com", "p.example.com", false)
                .reading_undeclared_user_agent()
                .into_provider(),
        ]);
        fixture.client.respond("p.example.com", 200, b"{}");

        let outcomes = fixture.run(&["p"], 1_000, None, None);

        assert!(
            matches!(outcome_of(&outcomes, "p"), CallOutcome::InputFailure),
            "should fail a provider that read an undeclared input, though it ignored the error"
        );
        assert!(
            fixture.client.sent_hosts().is_empty(),
            "should not send the call"
        );
    }

    #[test]
    fn generic_pull_dispatch_order_rotates_hourly() {
        let generic =
            |id: &str, host: &str| ScriptedProvider::new(id, host, host, true).into_provider();
        let mut fixture = DispatchFixture::new(vec![
            generic("gamma", "gamma.example.com"),
            generic("alpha", "alpha.example.com"),
            generic("beta", "beta.example.com"),
            scripted("other", "other.example.com", "other.example.com"),
        ]);
        fixture.settings.ec.identity.max_concurrency = 1;
        for host in ["alpha", "beta", "gamma", "other"] {
            fixture
                .client
                .respond(&format!("{host}.example.com"), 200, b"{}");
        }
        let ids = ["other", "gamma", "beta", "alpha"];

        fixture.run(&ids, 100, None, None);
        fixture.run(&ids, 3_600, None, None);
        fixture.run(&ids, 7_200, None, None);

        let sent = fixture.client.sent_hosts();
        assert_eq!(
            sent,
            [
                "alpha.example.com",
                "beta.example.com",
                "gamma.example.com",
                "other.example.com",
                "beta.example.com",
                "gamma.example.com",
                "alpha.example.com",
                "other.example.com",
                "gamma.example.com",
                "alpha.example.com",
                "beta.example.com",
                "other.example.com",
            ],
            "should sort generic pull by source, rotate the start each hour, and send other providers after"
        );
    }
}
