//! Identity work at response finalize: reconciling the identity cookies with
//! a KV read, queueing the writer's changes, and setting the cookies.
//!
//! Returning visitors never write KV here. The writer request adds every
//! change to its single mutation, which the after-send flush applies.
//! Stagers save positive changes in `__Host-ts-eids-s` and removals in a new
//! `__Host-ts-eids-r-*` cookie.

use edgezero_core::body::Body as EdgeBody;
use http::{HeaderValue, Response, header};

use super::catalog::ProviderCatalog;
use super::config::IdentitySettings;
use super::cookie::{
    CookieMergeContext, RBatch, RPlan, SCookie, SEntry, SOpen, Staging, cookie_view,
    delete_cookie_header, encode_s_cookie, merge_with_kv, plan_r_batch, r_batch_applied,
    r_batch_to_changes, set_cookie_header, split_r_batch,
};
use super::coordinator::{
    Eligibility, EligibilityRequest, EligibilityView, Phase, RequestFacts, claimed_sources,
    eligibility,
};
use super::descriptor::{Input, ResolveKind};
use super::ids::UnixTime;
use super::ids::{IdentityProviderId, SourceDomain};
use super::input::{ProviderConsent, Region, ResolveInput, ResolveSignals, StoredId};
use super::input::{RandomSource, ThreadRandom};
use super::mutation::{ApplyContext, IdentityMutation, PartnerIdChange, ProviderStateChange};
use super::policy::{PolicySignals, policy_allows};
use super::provider::IdentityProvider;
use super::resolution::IdentityProviderError;
use super::revision::{IdentityKeys, Revision, invalidated_uid_hash};
use crate::consent::allows_eid_persistence;
use crate::constants::COOKIE_TS_EIDS_S;
use crate::ec::EidSyncSource;
use crate::ec::consent::ec_consent_granted;
use crate::ec::kv::{KvIdentityGraph, PartnerIdUpdate, RemovalWriteOutcome};
use crate::ec::kv_types::{KvEntry, KvPartnerId};
use crate::ec::prebid_eids::collect_eid_updates;
use crate::ec::registry::PartnerRegistry;
use crate::ec::{EcContext, EcKvSnapshot};
use crate::settings::Settings;
use std::collections::BTreeMap;
use std::sync::Arc;

/// What identity finalize needs besides the request: keys, the provider
/// catalog, the `[ec.identity]` settings, the time and a random source.
pub(crate) struct IdentityFinalize {
    pub(crate) keys: IdentityKeys,
    pub(crate) catalog: ProviderCatalog,
    pub(crate) identity: IdentitySettings,
    /// Unix seconds.
    pub(crate) now: u64,
    random: Box<dyn RandomSource>,
}

impl IdentityFinalize {
    /// Built from settings and the partner registry. `None` when the clock is
    /// unavailable, because a zero time would make every expiry check pass.
    #[must_use]
    pub(crate) fn from_settings(settings: &Settings, registry: &PartnerRegistry) -> Option<Self> {
        let now = crate::ec::checked_current_timestamp()?;
        let catalog = ProviderCatalog::from_partners(registry).unwrap_or_else(|error| {
            log::warn!(
                "Identity providers unavailable: {}",
                error.current_context()
            );
            ProviderCatalog::empty()
        });
        Some(Self {
            keys: IdentityKeys::from_settings(settings),
            catalog,
            identity: settings.ec.identity.clone(),
            now,
            random: Box::new(ThreadRandom),
        })
    }

    /// A deterministic context for tests.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn for_test(catalog: ProviderCatalog, now: u64) -> Self {
        Self {
            keys: super::test_support::test_keys(),
            catalog,
            identity: IdentitySettings::default(),
            now,
            random: Box::new(super::input::SeededRandom::new(1)),
        }
    }

    /// The random source.
    #[must_use]
    pub(crate) fn random(&self) -> &dyn RandomSource {
        self.random.as_ref()
    }
}

/// Opens the identity cookies for the request's EC, once.
pub(crate) fn open_identity_cookies(context: &mut EcContext, finalize: &IdentityFinalize) {
    let Some(ec_id) = context.ec_value().map(str::to_owned) else {
        return;
    };
    context
        .identity_mut()
        .open_cookies(&finalize.keys, &finalize.catalog, &ec_id);
}

/// The inputs this request can supply to providers.
fn available_inputs(context: &EcContext) -> std::collections::BTreeSet<Input> {
    let identity = context.identity();
    let mut inputs = std::collections::BTreeSet::new();
    if context.client_ip().is_some() {
        inputs.insert(Input::ClientIp);
    }
    if identity.user_agent().is_some() {
        inputs.insert(Input::UserAgent);
    }
    if identity.page_url().is_some() {
        inputs.insert(Input::PageUrl);
    }
    if region_of(context).is_some() {
        inputs.insert(Input::Region);
    }
    inputs
}

fn region_of(context: &EcContext) -> Option<Region> {
    let geo = context.geo_info()?;
    Region::from_geo(&geo.country, geo.region.as_deref())
}

/// The facts of the writer request, for after-send eligibility.
#[must_use]
pub(crate) fn writer_facts(context: &EcContext, finalize: &IdentityFinalize) -> RequestFacts {
    RequestFacts {
        is_real_browser: context.identity().writer(),
        ec_persistence_allowed: ec_consent_granted(context.consent())
            && allows_eid_persistence(context.consent()),
        policy_signals: PolicySignals {
            dnt: context.identity().dnt(),
        },
        writer: context.identity().writer(),
        new_visitor: context.ec_generated(),
        runs_auction: false,
        now: UnixTime::from_secs(finalize.now),
        available_inputs: available_inputs(context),
    }
}

/// An after-send provider eligible in `phase`, with the sources to resolve.
pub(crate) struct Candidate {
    pub(crate) provider: Arc<dyn IdentityProvider>,
    pub(crate) kinds: BTreeMap<SourceDomain, ResolveKind>,
}

impl Candidate {
    fn stateful(&self) -> bool {
        self.provider
            .descriptor()
            .resolves()
            .is_some_and(super::descriptor::Resolves::has_private_state)
    }

    fn generic_pull(&self) -> bool {
        self.provider
            .descriptor()
            .resolves()
            .is_some_and(super::descriptor::Resolves::gets_ec_id)
    }

    fn has_unowned_source(&self, catalog: &ProviderCatalog) -> bool {
        let id = self.provider.descriptor().id();
        self.kinds
            .keys()
            .any(|source| catalog.owner_of(source) != Some(id))
    }
}

/// Every provider eligible in `phase` on `view`. A valid pull-complete
/// marker leaves generic pull providers out, as it always has.
pub(crate) fn candidates(
    context: &EcContext,
    finalize: &IdentityFinalize,
    view: &EligibilityView,
    phase: Phase,
) -> Vec<Candidate> {
    let facts = writer_facts(context, finalize);
    let request = EligibilityRequest {
        view,
        facts: &facts,
        consent: context.consent(),
        catalog: &finalize.catalog,
        keys: &finalize.keys,
    };
    let pull_complete = context.pull_sync_marker().is_valid();
    finalize
        .catalog
        .providers()
        .filter_map(|provider| {
            let settings = finalize
                .catalog
                .provider_settings(&finalize.identity, provider.descriptor().id());
            match eligibility(provider.as_ref(), &settings, phase, &request) {
                Eligibility::Eligible { kinds } => Some(Candidate {
                    provider: Arc::clone(provider),
                    kinds,
                }),
                Eligibility::Skip(_) => None,
            }
        })
        .filter(|candidate| !(pull_complete && candidate.generic_pull()))
        .collect()
}

fn opened_batches(context: &EcContext) -> Vec<RBatch> {
    context
        .identity()
        .opened()
        .map(|opened| {
            opened
                .removals
                .cookies
                .iter()
                .map(|(_, batch)| batch.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the writer must read KV before send (spec 6.3, step 2): the
/// cookie is missing, unreadable, from another config, stale, marked
/// missing, holds staged entries or markers, or removal cookies are present.
#[must_use]
pub(crate) fn phase1_read_needed(context: &EcContext, finalize: &IdentityFinalize) -> bool {
    let Some(opened) = context.identity().opened() else {
        return false;
    };
    if !opened.removals.cookies.is_empty() || !opened.removals.unopenable.is_empty() {
        return true;
    }
    let cookie = match &opened.s {
        SOpen::Missing | SOpen::Failed | SOpen::ConfigChanged(_) => return true,
        SOpen::Valid(cookie) => cookie,
    };
    if cookie.m
        || finalize.now.saturating_sub(cookie.kc) > finalize.identity.kv_revalidate_sec
        || cookie.ids.iter().any(|entry| !entry.k && !entry.d)
        || cookie.ps.values().any(|state| state.p.is_some())
    {
        return true;
    }
    // An after-send provider with an unowned candidate source needs KV for
    // its dry run, because the cookie caches no unowned values.
    let view = EligibilityView::merge(
        None,
        Some(cookie),
        &opened_batches(context),
        &finalize.catalog,
        &finalize.keys,
    );
    candidates(context, finalize, &view, Phase::AfterSendDryRun)
        .iter()
        .any(|candidate| !candidate.stateful() && candidate.has_unowned_source(&finalize.catalog))
}

/// Records, in the cookie only, that a read showed every unowned candidate
/// source of a provider present: `ps.n = now + refresh_after_sec`, with the
/// KV `n` it saw. Stops the unowned read trigger firing on every navigation.
fn apply_cookie_only_backoff(
    cookie: &mut SCookie,
    entry: &KvEntry,
    finalize: &IdentityFinalize,
) -> bool {
    let mut changed = false;
    for provider in finalize.catalog.providers() {
        let descriptor = provider.descriptor();
        let Some(resolves) = descriptor.resolves() else {
            continue;
        };
        if resolves.has_private_state() {
            continue;
        }
        let id = descriptor.id();
        let settings = finalize.catalog.provider_settings(&finalize.identity, id);
        let unowned: Vec<&SourceDomain> = claimed_sources(descriptor, &settings)
            .into_iter()
            .filter(|source| finalize.catalog.owner_of(source) != Some(id))
            .collect();
        let all_present = !unowned.is_empty()
            && unowned.iter().all(|source| {
                entry
                    .ids
                    .get(source.as_str())
                    .is_some_and(|record| record.exp.is_none_or(|exp| finalize.now < exp))
            });
        if !all_present {
            continue;
        }
        let kv_n = entry.ps.get(id.as_str()).and_then(|state| state.n);
        let state = cookie.ps.entry(id.to_string()).or_default();
        if state.n.is_some_and(|n| n > finalize.now) && state.kn == kv_n {
            continue;
        }
        state.n = Some(finalize.now + settings.refresh_after_sec);
        state.kn = kv_n;
        changed = true;
    }
    changed
}

/// A KV read answers a marker when every unowned candidate source of the
/// provider is now present, because the provider is then ineligible.
fn answered_by_present_sources(catalog: &ProviderCatalog) -> impl Fn(&str, &KvEntry) -> bool + '_ {
    move |provider, entry| {
        provider
            .parse::<IdentityProviderId>()
            .ok()
            .and_then(|id| catalog.get(&id).map(|found| (id, found)))
            .is_some_and(|(id, found)| {
                found.descriptor().sources().iter().all(|source| {
                    catalog.owner_of(source) != Some(&id) && entry.ids.contains_key(source.as_str())
                })
            })
    }
}

/// Reconciles the identity cookies with the request's KV read (spec 6.3,
/// step 3) and stages the cookie changes. A request that did not read KV
/// changes nothing.
pub(crate) fn reconcile_with_snapshot(context: &mut EcContext, finalize: &IdentityFinalize) {
    let snapshot = context.kv_snapshot().clone();
    if matches!(
        snapshot,
        EcKvSnapshot::NotRead | EcKvSnapshot::Failed { .. }
    ) {
        return;
    }
    let Some(opened) = context.identity().opened().cloned() else {
        return;
    };
    let writer = context.identity().writer();
    let cookie = opened
        .s
        .cookie()
        .cloned()
        .unwrap_or_else(|| SCookie::new(&finalize.catalog, 0));
    let answered = answered_by_present_sources(&finalize.catalog);
    let merge_context = CookieMergeContext {
        catalog: &finalize.catalog,
        keys: finalize.keys.clone(),
        now: finalize.now,
        kv_revalidate_sec: finalize.identity.kv_revalidate_sec,
        answered_by_eligibility: &answered,
    };
    let outcome = merge_with_kv(&cookie, &snapshot, &merge_context);
    let staged = context.identity_mut().staged_mut();

    if outcome.expire {
        staged.expire_s = matches!(
            opened.s,
            SOpen::Valid(_) | SOpen::ConfigChanged(_) | SOpen::Failed
        );
        staged.s_cookie = None;
        staged.delete_r = opened
            .removals
            .cookies
            .iter()
            .map(|(name, _)| name.clone())
            .chain(opened.removals.unopenable.iter().cloned())
            .collect();
        return;
    }
    let mut cookie = outcome.cookie;
    let backed_off = match &snapshot {
        EcKvSnapshot::Present { entry, .. } => {
            apply_cookie_only_backoff(&mut cookie, entry, finalize)
        }
        _ => false,
    };
    if outcome.changed || backed_off || !matches!(opened.s, SOpen::Valid(_)) {
        staged.s_cookie = Some(cookie);
    }
    if writer && let EcKvSnapshot::Present { entry, .. } = &snapshot {
        for (name, batch) in &opened.removals.cookies {
            if r_batch_applied(batch, entry, &finalize.keys) && !staged.delete_r.contains(name) {
                staged.delete_r.push(name.clone());
            }
        }
        for name in &opened.removals.unopenable {
            if !staged.delete_r.contains(name) {
                staged.delete_r.push(name.clone());
            }
        }
    }
}

/// Saves the `/auction` KV gap into `__Host-ts-eids-s` (spec 5.3): each
/// registered source in the consent-gated body set that the live row lacks
/// or holds only expired, unless its owner invalidated that UID. Nothing is
/// saved for a cookie-sourced client set, a failed or missing read, or a
/// tombstone.
pub(crate) fn stage_kv_gap(
    context: &mut EcContext,
    finalize: &IdentityFinalize,
    registry: &PartnerRegistry,
) {
    if context.eid_sync_source() != Some(EidSyncSource::Auction)
        || !context.identity().client_eids_from_body()
        || !allows_eid_persistence(context.consent())
    {
        return;
    }
    let Some(ec_id) = context.ec_value() else {
        return;
    };
    let Some(entry) = context.kv_snapshot().entry_for(ec_id) else {
        return;
    };
    if !entry.consent.ok {
        return;
    }
    let Some(eids) = context.client_eids() else {
        return;
    };
    let sources = finalize.catalog.configured_sources();
    let mut gap = Vec::new();
    for update in collect_eid_updates(None, None, Some(eids), registry) {
        let Ok(source) = update.partner_id.parse::<SourceDomain>() else {
            continue;
        };
        let Some(index) = sources.iter().position(|known| *known == source) else {
            continue;
        };
        let filled = entry
            .ids
            .get(source.as_str())
            .is_some_and(|record| record.exp.is_none_or(|exp| finalize.now < exp));
        if filled {
            continue;
        }
        let rejected = finalize.catalog.owner_of(&source).is_some_and(|owner| {
            entry.ps.get(owner.as_str()).is_some_and(|state| {
                state
                    .iv
                    .contains(&invalidated_uid_hash(&finalize.keys, owner, &update.uid))
            })
        });
        if !rejected {
            gap.push(SEntry::staged_browser(index, &update.uid, finalize.now));
        }
    }
    if gap.is_empty() {
        return;
    }

    let base = context
        .identity()
        .staged()
        .s_cookie
        .clone()
        .or_else(|| {
            context
                .identity()
                .opened()
                .and_then(|opened| opened.s.cookie().cloned())
        })
        .unwrap_or_else(|| SCookie::new(&finalize.catalog, finalize.now));
    let mut cookie = base;
    for entry in gap {
        let already = cookie
            .ids
            .iter()
            .any(|existing| existing.s == entry.s && existing.is_browser_gap());
        if !already {
            cookie.ids.push(entry);
        }
    }
    context.identity_mut().staged_mut().s_cookie = Some(cookie);
}

fn fill_missing_from_update(update: &PartnerIdUpdate) -> Option<PartnerIdChange> {
    Some(PartnerIdChange::FillMissing {
        source: update.partner_id.parse::<SourceDomain>().ok()?,
        record: KvPartnerId::browser(update.uid.clone(), update.acquired_at),
        writer: None,
        staged_at: None,
    })
}

/// The flush checks for one positive staged entry (spec 5.3). `Some` is the
/// change it becomes.
fn flush_checked_change(
    context: &EcContext,
    finalize: &IdentityFinalize,
    view: &EligibilityView,
    entry: (&SourceDomain, &KvPartnerId, &Staging),
) -> Option<PartnerIdChange> {
    let (source, record, staging) = entry;
    let Staging::Staged { pv, sa, browser } = staging else {
        return None;
    };
    let staged_at = sa.unwrap_or(0);
    if finalize.now.saturating_sub(staged_at) > finalize.identity.staged_max_age_sec {
        return None;
    }
    if !allows_eid_persistence(context.consent()) {
        return None;
    }
    if *browser {
        return Some(PartnerIdChange::FillMissing {
            source: source.clone(),
            record: record.clone(),
            writer: None,
            staged_at: *sa,
        });
    }

    let writer = record.w.as_deref()?.parse::<IdentityProviderId>().ok()?;
    let provider = finalize.catalog.get(&writer)?;
    let settings = finalize
        .catalog
        .provider_settings(&finalize.identity, &writer);
    if !claimed_sources(provider.descriptor(), &settings).contains(&source) {
        return None;
    }
    let owner = finalize.catalog.owner_of(source);
    if owner.is_some_and(|owner| owner != &writer) {
        return None;
    }
    let signals = PolicySignals {
        dnt: context.identity().dnt(),
    };
    if !policy_allows(
        provider.descriptor().consent_policy(),
        context.consent(),
        signals,
    ) {
        return None;
    }
    if view.is_opted_out(&writer) {
        return None;
    }
    let mut record = record.clone();
    record.w = None;
    record.rv = None;
    Some(if owner == Some(&writer) {
        PartnerIdChange::OwnerWrite {
            source: source.clone(),
            record,
            writer,
            expected: pv.as_deref().map(Revision::from_stored),
            staged_at: *sa,
        }
    } else {
        PartnerIdChange::FillMissing {
            source: source.clone(),
            record,
            writer: Some(writer),
            staged_at: *sa,
        }
    })
}

/// Adds the writer's changes to its single mutation: browser ingest, the
/// flush-checked staged entries of `__Host-ts-eids-s`, and every removal
/// batch. Removals skip every check but the EC binding (spec 5.6).
pub(crate) fn queue_writer_changes(
    context: &mut EcContext,
    finalize: &IdentityFinalize,
    ingest: &[PartnerIdUpdate],
) {
    let mut changes: Vec<PartnerIdChange> =
        ingest.iter().filter_map(fill_missing_from_update).collect();
    let mut states: Vec<(IdentityProviderId, ProviderStateChange)> = Vec::new();

    if let Some(opened) = context.identity().opened().cloned() {
        let batches: Vec<RBatch> = opened
            .removals
            .cookies
            .iter()
            .map(|(_, batch)| batch.clone())
            .collect();
        let kv_entry = context
            .ec_value()
            .and_then(|ec_id| context.kv_snapshot().entry_for(ec_id));
        if let Some(cookie) = opened.s.cookie() {
            let view = EligibilityView::merge(
                kv_entry,
                Some(cookie),
                &batches,
                &finalize.catalog,
                &finalize.keys,
            );
            for (source, record, staging) in cookie_view(cookie, &finalize.catalog) {
                if let Some(change) =
                    flush_checked_change(context, finalize, &view, (&source, &record, &staging))
                {
                    changes.push(change);
                }
            }
            for (provider, state) in &cookie.ps {
                let Ok(provider) = provider.parse::<IdentityProviderId>() else {
                    continue;
                };
                let staged = ProviderStateChange {
                    n: state.n.filter(|_| state.sg),
                    f: state.f.filter(|_| state.sg),
                    opt_out_clear: state.oc,
                    ..ProviderStateChange::default()
                };
                if staged != ProviderStateChange::default() {
                    states.push((provider, staged));
                }
            }
        }
        for batch in &batches {
            let (removals, batch_states) = r_batch_to_changes(batch);
            changes.extend(removals);
            states.extend(batch_states);
        }
    }

    let mutation = context.identity_mut().mutation_mut();
    for change in changes {
        mutation.add_change(change);
    }
    for (provider, state) in states {
        mutation.merge_state(provider, state);
    }
}

/// The signals a provider call may use, from the request context.
pub(crate) fn request_signals<'a>(
    context: &'a EcContext,
    finalize: &'a IdentityFinalize,
    publisher_domain: &'a str,
    client_ip: Option<std::net::IpAddr>,
) -> ResolveSignals<'a> {
    ResolveSignals {
        client_ip,
        user_agent: context.identity().user_agent(),
        page_url: context.identity().page_url().cloned(),
        region: region_of(context),
        hashed_email: None,
        consent: ProviderConsent::from_context(context.consent()),
        publisher_domain,
        now: UnixTime::from_secs(finalize.now),
        random: finalize.random(),
        ec_id: context.ec_value().map(str::to_owned),
        stored: BTreeMap::new(),
        state_json: None,
        dry_run: false,
    }
}

/// A provider's stored records and private state in a row or view.
pub(crate) fn provider_records(
    provider: &dyn IdentityProvider,
    view: &EligibilityView,
) -> (BTreeMap<SourceDomain, StoredId>, Option<String>) {
    let stored = provider
        .descriptor()
        .sources()
        .iter()
        .filter_map(|source| {
            view.record(source)
                .map(|record| (source.clone(), StoredId::from_record(record)))
        })
        .collect();
    let state = view
        .state(provider.descriptor().id().as_str())
        .and_then(|state| state.st.as_ref())
        .and_then(|st| serde_json::to_string(st).ok());
    (stored, state)
}

/// Phase 3 on the writer (spec 5.2): dry-runs each eligible after-send
/// provider and marks the ones that would call, so phase 4 knows to re-read
/// the row. A provider with private state gets no dry run and is marked when
/// due. An unowned dry run needs a KV read that showed the source missing.
pub(crate) fn mark_after_send(
    context: &mut EcContext,
    finalize: &IdentityFinalize,
    publisher_domain: &str,
    status: http::StatusCode,
) {
    let returning = context.ec_was_present() && !context.ec_generated();
    if !context.identity().writer()
        || !returning
        || !(status.is_success() || status == http::StatusCode::NOT_MODIFIED)
    {
        return;
    }
    let kv_read = context
        .ec_value()
        .and_then(|ec_id| context.kv_snapshot().entry_for(ec_id))
        .is_some();
    let cookie = context.identity().staged().s_cookie.clone().or_else(|| {
        context
            .identity()
            .opened()
            .and_then(|opened| opened.s.cookie().cloned())
    });
    let view = EligibilityView::merge(
        context
            .ec_value()
            .and_then(|ec_id| context.kv_snapshot().entry_for(ec_id)),
        cookie.as_ref(),
        &opened_batches(context),
        &finalize.catalog,
        &finalize.keys,
    );

    let mut marked = Vec::new();
    let mut skipped = Vec::new();
    for candidate in candidates(context, finalize, &view, Phase::AfterSendDryRun) {
        let id = candidate.provider.descriptor().id().clone();
        if candidate.stateful() {
            marked.push(id);
            continue;
        }
        if candidate.has_unowned_source(&finalize.catalog) && !kv_read {
            continue;
        }
        let (stored, state) = provider_records(candidate.provider.as_ref(), &view);
        let base = request_signals(context, finalize, publisher_domain, None);
        let client_ip = context
            .client_ip()
            .and_then(|ip| ip.parse::<std::net::IpAddr>().ok());
        let signals = ResolveSignals {
            client_ip,
            ..base.for_provider(stored, state, true)
        };
        let input = ResolveInput::new(candidate.provider.descriptor(), candidate.kinds, &signals);
        match candidate.provider.request(&input) {
            Ok(Some(_)) => marked.push(id),
            Ok(None) => skipped.push(id),
            Err(error)
                if matches!(
                    error.current_context(),
                    IdentityProviderError::InputUnavailable { .. }
                ) =>
            {
                skipped.push(id);
            }
            Err(error) => log::warn!(
                "Identity provider {id} dry run failed: {}",
                error.current_context()
            ),
        }
    }

    for id in skipped {
        let settings = finalize.catalog.provider_settings(&finalize.identity, &id);
        context.identity_mut().mutation_mut().merge_state(
            id,
            ProviderStateChange {
                n: Some(finalize.now + settings.refresh_after_sec),
                ..ProviderStateChange::default()
            },
        );
    }
    if marked.is_empty() {
        return;
    }
    let mut cookie = cookie.unwrap_or_else(|| SCookie::new(&finalize.catalog, finalize.now));
    for id in marked {
        cookie.ps.entry(id.to_string()).or_default().p = Some(finalize.now);
        context.identity_mut().mark(id);
    }
    context.identity_mut().staged_mut().s_cookie = Some(cookie);
}

/// The removals, opt-outs, clears and `iv` hashes of every removal cookie,
/// for an orphan rotation to carry into the new row.
#[must_use]
pub(crate) fn rotation_removals(context: &EcContext) -> IdentityMutation {
    let mut mutation = IdentityMutation::default();
    if let Some(opened) = context.identity().opened() {
        for (_, batch) in &opened.removals.cookies {
            let (removals, states) = r_batch_to_changes(batch);
            for change in removals {
                mutation.add_change(change);
            }
            for (provider, state) in states {
                mutation.merge_state(provider, state);
            }
        }
    }
    mutation.removals_only()
}

/// After a rotation's create was written: the removal cookies are deleted,
/// because the new row carries them, and `__Host-ts-eids-s` is rebuilt from
/// the created row. The rest of the mutation and every marker are dropped.
pub(crate) fn after_rotation(context: &mut EcContext, finalize: &IdentityFinalize) {
    let names: Vec<String> = context
        .identity()
        .opened()
        .map(|opened| {
            opened
                .removals
                .cookies
                .iter()
                .map(|(name, _)| name.clone())
                .chain(opened.removals.unopenable.iter().cloned())
                .collect()
        })
        .unwrap_or_default();
    let snapshot = context.kv_snapshot().clone();
    let never = |_: &str, _: &KvEntry| false;
    let merge_context = CookieMergeContext {
        catalog: &finalize.catalog,
        keys: finalize.keys.clone(),
        now: finalize.now,
        kv_revalidate_sec: finalize.identity.kv_revalidate_sec,
        answered_by_eligibility: &never,
    };
    let rebuilt = merge_with_kv(
        &SCookie::new(&finalize.catalog, finalize.now),
        &snapshot,
        &merge_context,
    )
    .cookie;
    let identity = context.identity_mut();
    *identity.mutation_mut() = IdentityMutation::default();
    let staged = identity.staged_mut();
    staged.s_cookie = Some(rebuilt);
    staged.expire_s = false;
    staged.delete_r = names;
}

/// Stages `__Host-ts-eids-s` for a newly created row.
pub(crate) fn stage_new_ec_cookie(context: &mut EcContext, finalize: &IdentityFinalize) {
    let snapshot = context.kv_snapshot().clone();
    let never = |_: &str, _: &KvEntry| false;
    let merge_context = CookieMergeContext {
        catalog: &finalize.catalog,
        keys: finalize.keys.clone(),
        now: finalize.now,
        kv_revalidate_sec: finalize.identity.kv_revalidate_sec,
        answered_by_eligibility: &never,
    };
    let cookie = merge_with_kv(
        &SCookie::new(&finalize.catalog, finalize.now),
        &snapshot,
        &merge_context,
    )
    .cookie;
    context.identity_mut().staged_mut().s_cookie = Some(cookie);
}

/// Expires `__Host-ts-eids-s` when the request carried one, and on request
/// deletes every removal cookie, for a withdrawal or unusable consent.
pub(crate) fn stage_consent_loss(context: &mut EcContext, delete_removals: bool) {
    let has_s = context.identity().raw_cookies().has_s();
    let names = context.identity().raw_cookies().r_names();
    let staged = context.identity_mut().staged_mut();
    staged.s_cookie = None;
    staged.expire_s = has_s;
    if delete_removals {
        staged.delete_r = names;
    }
}

fn append_set_cookie(response: &mut Response<EdgeBody>, value: &str) {
    match HeaderValue::from_str(value) {
        Ok(value) => {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
        Err(_) => log::warn!("Dropping identity cookie that is not a valid header value"),
    }
}

/// A removal batch as a removals-only mutation.
fn batch_mutation(batch: &RBatch) -> IdentityMutation {
    let (removals, states) = r_batch_to_changes(batch);
    let mut mutation = IdentityMutation::default();
    for change in removals {
        mutation.add_change(change);
    }
    for (provider, state) in states {
        mutation.merge_state(provider, state);
    }
    mutation.removals_only()
}

/// Sets and deletes the identity cookies this response staged. The only
/// place `__Host-ts-eids-*` cookies are written.
///
/// A removal batch that does not fit one cookie, or meets four removal
/// cookies already present, is written to KV before send. Only when that
/// write fails is it set in new cookies of its own, split but never
/// coarsened.
pub(crate) fn emit_identity_cookies(
    context: &mut EcContext,
    finalize: &IdentityFinalize,
    graph: Option<&KvIdentityGraph>,
    response: &mut Response<EdgeBody>,
) {
    let ec_id = context.ec_value().map(str::to_owned);
    let inbound_openable = context
        .identity()
        .opened()
        .map_or(0, |opened| opened.removals.cookies.len());
    let staged = context.identity().staged().clone();

    if staged.expire_s {
        append_set_cookie(response, &delete_cookie_header(COOKIE_TS_EIDS_S));
    } else if let (Some(mut cookie), Some(ec_id)) = (staged.s_cookie, ec_id.as_deref()) {
        let priority = |provider: &str| {
            provider
                .parse::<IdentityProviderId>()
                .map(|id| {
                    finalize
                        .catalog
                        .provider_settings(&finalize.identity, &id)
                        .cookie_priority
                })
                .unwrap_or_default()
        };
        let value = encode_s_cookie(
            &finalize.keys,
            ec_id,
            &mut cookie,
            finalize.random(),
            &priority,
        );
        append_set_cookie(response, &set_cookie_header(COOKIE_TS_EIDS_S, &value));
    }

    if !staged.new_r_batch.is_empty()
        && let Some(ec_id) = ec_id.as_deref()
    {
        let cookies = match plan_r_batch(
            &finalize.keys,
            ec_id,
            &staged.new_r_batch,
            inbound_openable,
            finalize.random(),
        ) {
            RPlan::Cookie { name, value } => vec![(name, value)],
            RPlan::Overflow => {
                let outcome = graph.map_or(RemovalWriteOutcome::Failed, |graph| {
                    graph.apply_removals_now(
                        ec_id,
                        &batch_mutation(&staged.new_r_batch),
                        &ApplyContext {
                            now: finalize.now,
                            keys: &finalize.keys,
                            random: finalize.random(),
                            catalog: &finalize.catalog,
                        },
                    )
                });
                match outcome {
                    RemovalWriteOutcome::Written | RemovalWriteOutcome::AlreadyApplied => {
                        Vec::new()
                    }
                    RemovalWriteOutcome::Failed => split_r_batch(
                        &finalize.keys,
                        ec_id,
                        &staged.new_r_batch,
                        finalize.random(),
                    ),
                }
            }
        };
        for (name, value) in cookies {
            append_set_cookie(response, &set_cookie_header(&name, &value));
        }
    }

    for name in &staged.delete_r {
        append_set_cookie(response, &delete_cookie_header(name));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use http::{HeaderMap, HeaderValue, header};

    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource};
    use crate::ec::current_timestamp;
    use crate::ec::finalize::ec_finalize_response;
    use crate::ec::identity::cookie::{RRemoval, seal};
    use crate::ec::identity::input::SeededRandom;
    use crate::ec::identity::test_support::test_keys;
    use crate::ec::kv::KvIdentityGraph;
    use crate::settings::EcPartner;
    use crate::test_support::tests::create_test_settings;

    fn ec_id() -> String {
        format!("{}.ABC123", "a".repeat(64))
    }

    fn partner(source: &str) -> EcPartner {
        EcPartner {
            name: format!("Partner {source}"),
            source_domain: source.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: None,
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: None,
        }
    }

    fn registry() -> PartnerRegistry {
        PartnerRegistry::from_config(&[partner("a.example.com"), partner("b.example.com")])
            .expect("should build registry")
    }

    fn consent(jurisdiction: Jurisdiction) -> ConsentContext {
        ConsentContext {
            jurisdiction,
            source: ConsentSource::Cookie,
            ..ConsentContext::default()
        }
    }

    fn sealed_s(cookie: &SCookie) -> String {
        seal(
            &test_keys().cookie_s,
            &ec_id(),
            &serde_json::to_vec(cookie).expect("should serialize"),
            &SeededRandom::new(5),
        )
    }

    fn sealed_r(batch: &RBatch) -> String {
        seal(
            &test_keys().cookie_r,
            &ec_id(),
            &serde_json::to_vec(batch).expect("should serialize"),
            &SeededRandom::new(6),
        )
    }

    fn context_with(cookies: &str, writer: bool, jurisdiction: Jurisdiction) -> EcContext {
        let mut context = EcContext::new_for_test(Some(ec_id()), consent(jurisdiction));
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(cookies).expect("should build cookie header"),
        );
        context.identity_mut().set_raw_cookies_for_test(&headers);
        context.identity_mut().set_writer(writer);
        context
    }

    fn set_cookies(response: &Response<EdgeBody>) -> Vec<String> {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok().map(str::to_owned))
            .collect()
    }

    fn removal_batch(writer: &str) -> RBatch {
        RBatch {
            rm: vec![RRemoval {
                s: "a.example.com".to_owned(),
                w: writer.to_owned(),
                pv: None,
                sa: current_timestamp(),
            }],
            o: [("pull-b-example-com".to_owned(), current_timestamp())]
                .into_iter()
                .collect(),
            ..RBatch::default()
        }
    }

    #[test]
    fn flush_checks_drop_stale_and_unknown_writer_entries_but_never_removals() {
        let settings = create_test_settings();
        let registry = registry();
        let catalog = ProviderCatalog::from_partners(&registry).expect("should build catalog");
        let now = current_timestamp();
        let mut cookie = SCookie::new(&catalog, now);
        cookie.ids.push(SEntry::staged_browser(0, "fresh", now));
        cookie.ids.push(SEntry::staged_browser(
            1,
            "stale",
            now.saturating_sub(8 * 86_400),
        ));
        cookie.ids.push(SEntry::staged_provider(
            1,
            &KvPartnerId {
                w: Some("gone".to_owned()),
                ..KvPartnerId::new("from-a-disabled-provider")
            },
            None,
            now,
        ));
        let cookies = format!(
            "__Host-ts-eids-s={}; __Host-ts-eids-r-0a1b2c3d={}",
            sealed_s(&cookie),
            sealed_r(&removal_batch("gone"))
        );
        let graph = KvIdentityGraph::in_memory("ec-store");
        graph
            .create(&ec_id(), &KvEntry::minimal("z.example.com", "x", 1))
            .expect("should seed");
        let mut context = context_with(&cookies, true, Jurisdiction::NonRegulated);
        context.set_kv_snapshot(graph.load_snapshot(&ec_id()));
        let mut response = Response::new(EdgeBody::empty());

        ec_finalize_response(
            &settings,
            &mut context,
            Some(&graph),
            &registry,
            None,
            None,
            &mut response,
        );

        let changes = context.identity().mutation().changes();
        assert!(
            changes.iter().any(|change| matches!(
                change,
                PartnerIdChange::FillMissing { record, .. } if record.uid == "fresh"
            )),
            "a fresh browser entry is flushed"
        );
        assert!(
            !changes.iter().any(|change| matches!(
                change,
                PartnerIdChange::FillMissing { record, .. }
                    if record.uid == "stale" || record.uid == "from-a-disabled-provider"
            )),
            "a stale entry and one from an unknown writer are dropped"
        );
        assert!(
            changes.iter().any(|change| matches!(
                change,
                PartnerIdChange::WriterRemove { writer, .. } if writer.as_str() == "gone"
            )),
            "a removal survives even for a writer that is no longer configured"
        );
        assert!(
            context.identity().mutation().has_removals(),
            "the staged opt-out joins the mutation too"
        );
    }

    #[test]
    fn tombstone_read_expires_the_cookie_and_deletes_removal_cookies() {
        let settings = create_test_settings();
        let registry = registry();
        let catalog = ProviderCatalog::from_partners(&registry).expect("should build catalog");
        let cookies = format!(
            "__Host-ts-eids-s={}; __Host-ts-eids-r-0a1b2c3d={}",
            sealed_s(&SCookie::new(&catalog, current_timestamp())),
            sealed_r(&removal_batch("pull-a-example-com"))
        );
        let mut context = context_with(&cookies, false, Jurisdiction::NonRegulated);
        context.set_kv_snapshot(EcKvSnapshot::Present {
            ec_id: ec_id(),
            entry: Box::new(KvEntry::tombstone(1)),
            generation: Some(1),
        });
        let graph = KvIdentityGraph::in_memory("ec-store");
        let mut response = Response::new(EdgeBody::empty());

        ec_finalize_response(
            &settings,
            &mut context,
            Some(&graph),
            &registry,
            None,
            None,
            &mut response,
        );

        let cookies = set_cookies(&response);
        assert!(
            cookies
                .iter()
                .any(|value| value.starts_with("__Host-ts-eids-s=;") && value.contains("Max-Age=0")),
            "expires ts-eids-s: {cookies:?}"
        );
        assert!(
            cookies
                .iter()
                .any(|value| value.starts_with("__Host-ts-eids-r-0a1b2c3d=;")),
            "deletes the removal cookie the tombstone covers"
        );
    }

    #[test]
    fn stager_keeps_removal_cookies_and_sets_none_of_its_own_without_removals() {
        let settings = create_test_settings();
        let registry = registry();
        let cookies = format!(
            "__Host-ts-eids-r-0a1b2c3d={}",
            sealed_r(&removal_batch("pull-a-example-com"))
        );
        let graph = KvIdentityGraph::in_memory("ec-store");
        graph
            .create(&ec_id(), &KvEntry::minimal("z.example.com", "x", 1))
            .expect("should seed");
        let mut context = context_with(&cookies, false, Jurisdiction::NonRegulated);
        context.set_kv_snapshot(graph.load_snapshot(&ec_id()));
        let mut response = Response::new(EdgeBody::empty());

        ec_finalize_response(
            &settings,
            &mut context,
            Some(&graph),
            &registry,
            None,
            None,
            &mut response,
        );

        assert!(
            !set_cookies(&response)
                .iter()
                .any(|value| value.starts_with("__Host-ts-eids-r-")),
            "a stager never deletes or adds a removal cookie without removals"
        );
        assert!(
            context.identity().mutation().is_empty(),
            "a stager queues nothing"
        );
    }

    #[test]
    fn unusable_consent_expires_ts_eids_s_and_keeps_removals() {
        let settings = create_test_settings();
        let registry = registry();
        let catalog = ProviderCatalog::from_partners(&registry).expect("should build catalog");
        let cookies = format!(
            "__Host-ts-eids-s={}; __Host-ts-eids-r-0a1b2c3d={}",
            sealed_s(&SCookie::new(&catalog, current_timestamp())),
            sealed_r(&removal_batch("pull-a-example-com"))
        );
        let mut context = context_with(&cookies, true, Jurisdiction::Unknown);
        let graph = KvIdentityGraph::in_memory("ec-store");
        let mut response = Response::new(EdgeBody::empty());

        ec_finalize_response(
            &settings,
            &mut context,
            Some(&graph),
            &registry,
            None,
            None,
            &mut response,
        );

        let cookies = set_cookies(&response);
        assert!(
            cookies
                .iter()
                .any(|value| value.starts_with("__Host-ts-eids-s=;")),
            "expires ts-eids-s: {cookies:?}"
        );
        assert!(
            !cookies
                .iter()
                .any(|value| value.starts_with("__Host-ts-eids-r-")),
            "keeps the removal cookie until a writer applies it"
        );
    }

    fn big_batch(count: usize) -> RBatch {
        RBatch {
            rm: (0..count)
                .map(|index| RRemoval {
                    s: format!("s{index}.example.com"),
                    w: "p".to_owned(),
                    pv: None,
                    sa: current_timestamp(),
                })
                .collect(),
            ..RBatch::default()
        }
    }

    fn row_with_records(count: usize) -> KvEntry {
        let mut entry = KvEntry::minimal("z.example.com", "x", 1);
        for index in 0..count {
            entry.ids.insert(
                format!("s{index}.example.com"),
                KvPartnerId {
                    w: Some("p".to_owned()),
                    at: Some(1),
                    ..KvPartnerId::new("u")
                },
            );
        }
        entry
    }

    fn emit_with(graph: &KvIdentityGraph, cookies: &str, batch: RBatch) -> Response<EdgeBody> {
        let registry = registry();
        let finalize = IdentityFinalize::for_test(
            ProviderCatalog::from_partners(&registry).expect("should build catalog"),
            current_timestamp(),
        );
        let mut context = context_with(cookies, false, Jurisdiction::NonRegulated);
        open_identity_cookies(&mut context, &finalize);
        context.identity_mut().staged_mut().new_r_batch = batch;
        let mut response = Response::new(EdgeBody::empty());
        emit_identity_cookies(&mut context, &finalize, Some(graph), &mut response);
        response
    }

    fn removal_cookie_count(response: &Response<EdgeBody>) -> usize {
        set_cookies(response)
            .iter()
            .filter(|value| value.starts_with("__Host-ts-eids-r-") && !value.contains("Max-Age=0"))
            .count()
    }

    #[test]
    fn overflow_batch_is_written_before_send_with_no_cookie() {
        let (graph, counter) = KvIdentityGraph::write_counting("ec-store");
        graph
            .create(&ec_id(), &row_with_records(12))
            .expect("should seed");
        let before = counter.writes();

        let response = emit_with(&graph, "x=1", big_batch(12));

        assert_eq!(
            counter.writes().if_generation_match - before.if_generation_match,
            1,
            "one conditional write before send"
        );
        assert_eq!(
            removal_cookie_count(&response),
            0,
            "no removal cookie is set"
        );
        let (stored, _) = graph
            .get(&ec_id())
            .expect("should read")
            .expect("should exist");
        assert_eq!(
            stored.ids.len(),
            1,
            "every named record is removed, the control kept"
        );
    }

    #[test]
    fn overflow_retries_only_on_generation_mismatch() {
        let (graph, counter) = KvIdentityGraph::write_counting("ec-store");
        graph
            .create(&ec_id(), &row_with_records(12))
            .expect("should seed");
        counter.conflict_next(2);
        let before = counter.writes();

        let response = emit_with(&graph, "x=1", big_batch(12));

        assert_eq!(
            counter.writes().if_generation_match - before.if_generation_match,
            3,
            "two conflicts, then the write lands"
        );
        assert_eq!(
            removal_cookie_count(&response),
            0,
            "no removal cookie is set"
        );
    }

    #[test]
    fn overflow_write_failure_falls_back_to_split_cookies() {
        let graph = KvIdentityGraph::failing("ec-store");

        let response = emit_with(&graph, "x=1", big_batch(12));

        assert!(
            removal_cookie_count(&response) >= 2,
            "the batch is split across new cookies when the write fails"
        );
        assert!(
            !set_cookies(&response)
                .iter()
                .any(|value| value.starts_with("__Host-ts-eids-s=")),
            "no other cookie is rewritten"
        );
    }

    #[test]
    fn four_openable_removal_cookies_force_overflow() {
        let (graph, counter) = KvIdentityGraph::write_counting("ec-store");
        graph
            .create(&ec_id(), &row_with_records(1))
            .expect("should seed");
        let existing = sealed_r(&big_batch(1));
        let cookies = (0..4)
            .map(|index| format!("__Host-ts-eids-r-0000000{index}={existing}"))
            .collect::<Vec<_>>()
            .join("; ");
        let before = counter.writes();

        let response = emit_with(&graph, &cookies, big_batch(1));

        assert_eq!(
            counter.writes().if_generation_match - before.if_generation_match,
            1,
            "a small batch is written when four cookies are present"
        );
        assert_eq!(
            removal_cookie_count(&response),
            0,
            "and adds no fifth cookie"
        );
    }

    #[test]
    fn a_small_batch_gets_a_cookie_of_its_own() {
        let (graph, counter) = KvIdentityGraph::write_counting("ec-store");
        graph
            .create(&ec_id(), &row_with_records(1))
            .expect("should seed");
        let before = counter.writes();

        let response = emit_with(&graph, "x=1", big_batch(1));

        assert_eq!(counter.writes().total() - before.total(), 0, "no write");
        assert_eq!(removal_cookie_count(&response), 1, "one removal cookie");
    }

    fn pull_registry() -> PartnerRegistry {
        let mut pull = partner("ssp.example.com");
        pull.pull_sync_enabled = true;
        pull.pull_sync_url = Some("https://sync.ssp.example.com/pull".to_owned());
        pull.pull_sync_allowed_domains = vec!["sync.ssp.example.com".to_owned()];
        pull.ts_pull_token = Some(crate::redacted::Redacted::new(
            "example-token-value-0123456789abcdef".to_owned(),
        ));
        PartnerRegistry::from_config(&[pull]).expect("should build registry")
    }

    fn pull_finalize() -> IdentityFinalize {
        IdentityFinalize::for_test(
            ProviderCatalog::from_partners(&pull_registry()).expect("should build catalog"),
            current_timestamp(),
        )
    }

    #[test]
    fn unowned_pull_partner_is_marked_only_after_a_read_shows_it_missing() {
        let finalize = pull_finalize();
        let graph = KvIdentityGraph::in_memory("ec-store");
        graph
            .create(&ec_id(), &KvEntry::minimal("z.example.com", "x", 1))
            .expect("should seed");

        let mut unread = context_with("x=1", true, Jurisdiction::NonRegulated);
        open_identity_cookies(&mut unread, &finalize);
        mark_after_send(
            &mut unread,
            &finalize,
            "www.example.com",
            http::StatusCode::OK,
        );
        assert!(
            unread.identity().marked().is_empty(),
            "without a read the cookie cannot know the source is missing"
        );

        let mut read = context_with("x=1", true, Jurisdiction::NonRegulated);
        read.set_kv_snapshot(graph.load_snapshot(&ec_id()));
        open_identity_cookies(&mut read, &finalize);
        mark_after_send(
            &mut read,
            &finalize,
            "www.example.com",
            http::StatusCode::OK,
        );
        assert_eq!(
            read.identity()
                .marked()
                .iter()
                .map(IdentityProviderId::as_str)
                .collect::<Vec<_>>(),
            vec!["pull-ssp-example-com"],
            "a read showing the source missing marks the pull partner"
        );
        assert!(
            read.identity()
                .staged()
                .s_cookie
                .as_ref()
                .is_some_and(|cookie| cookie.ps["pull-ssp-example-com"].p.is_some()),
            "the marker goes into ts-eids-s"
        );

        let mut redirect = context_with("x=1", true, Jurisdiction::NonRegulated);
        redirect.set_kv_snapshot(graph.load_snapshot(&ec_id()));
        open_identity_cookies(&mut redirect, &finalize);
        mark_after_send(
            &mut redirect,
            &finalize,
            "www.example.com",
            http::StatusCode::FOUND,
        );
        assert!(
            redirect.identity().marked().is_empty(),
            "a 302 marks nothing"
        );
    }

    #[test]
    fn present_unowned_source_backs_off_the_read_trigger_in_the_cookie() {
        let finalize = pull_finalize();
        let mut entry = KvEntry::minimal("ssp.example.com", "held", 1);
        entry
            .ids
            .get_mut("ssp.example.com")
            .expect("should hold it")
            .at = Some(1);
        let mut context = context_with("x=1", true, Jurisdiction::NonRegulated);
        context.set_kv_snapshot(EcKvSnapshot::Present {
            ec_id: ec_id(),
            entry: Box::new(entry),
            generation: Some(1),
        });
        open_identity_cookies(&mut context, &finalize);
        assert!(
            phase1_read_needed(&context, &finalize),
            "control: with no cookie the writer reads"
        );

        reconcile_with_snapshot(&mut context, &finalize);
        let cookie = context
            .identity()
            .staged()
            .s_cookie
            .clone()
            .expect("should stage the cookie");
        assert!(
            cookie.ps["pull-ssp-example-com"]
                .n
                .is_some_and(|next| next > finalize.now),
            "the cookie records when the partner is next due"
        );

        let sealed = sealed_s(&cookie);
        let mut next = context_with(
            &format!("__Host-ts-eids-s={sealed}"),
            true,
            Jurisdiction::NonRegulated,
        );
        open_identity_cookies(&mut next, &finalize);
        assert!(
            !phase1_read_needed(&next, &finalize),
            "the next navigation skips the read"
        );
    }

    #[test]
    fn orphan_rotation_carries_removals_but_never_positive_staged_ids() {
        let registry = registry();
        let finalize = IdentityFinalize::for_test(
            ProviderCatalog::from_partners(&registry).expect("should build catalog"),
            current_timestamp(),
        );
        let mut context = context_with(
            &format!(
                "__Host-ts-eids-r-0a1b2c3d={}",
                sealed_r(&removal_batch("pull-a-example-com"))
            ),
            true,
            Jurisdiction::NonRegulated,
        );
        open_identity_cookies(&mut context, &finalize);
        let removals = rotation_removals(&context);
        assert!(removals.has_removals(), "carries the staged opt-out");
        assert!(
            removals.changes().iter().all(|change| matches!(
                change,
                PartnerIdChange::WriterRemove { .. } | PartnerIdChange::OwnerRemove { .. }
            )),
            "and only removals"
        );

        let entry = KvEntry::minimal("z.example.com", "x", 1);
        let applied = crate::ec::identity::mutation::apply(
            &removals,
            &entry,
            &crate::ec::identity::mutation::ApplyContext {
                now: finalize.now,
                keys: &finalize.keys,
                random: finalize.random(),
                catalog: &finalize.catalog,
            },
        )
        .expect("should apply");
        assert_eq!(
            applied.entry.ps,
            BTreeMap::new(),
            "an opt-out of an unconfigured provider is pruned with its state"
        );
    }
}
