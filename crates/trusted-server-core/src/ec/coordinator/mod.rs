//! Server-side identity lookup coordination (spec §8).
//!
//! [`ProviderCoordinator`] owns eligibility, consent, destination checks, a
//! deadline-bounded fan-out through a [`LookupTransport`], validation and
//! planning of the navigation's single row change. Providers stay pure.

pub mod results;
pub mod transport;
mod validate;

use std::collections::HashSet;
use std::num::{NonZeroU16, NonZeroU32};
use std::sync::Arc;
use std::time::Duration;

use error_stack::Report;

use self::results::{LookupResults, LookupStatus, ProviderLookup, SkipReason};
use self::transport::{
    Deadline, LookupTransport, ProviderIndex, Tagged, TransportError, TransportResponse,
};
use self::validate::{ValidatedEntries, ValidationPolicy, validate};
use crate::consent::ConsentContext;
use crate::consent::{allows_eid_persistence, tcf_vendor_consent_denied};
use crate::ec::EcKvSnapshot;
use crate::ec::consent::ec_consent_granted;
use crate::ec::eid_types::{ProviderId, SourceDomain, UnixSeconds};
use crate::ec::kv_types::{KvEid, KvEntry};
use crate::ec::page_load::RowChange;
use crate::ec::provider::{
    ClientSignals, CookieRead, IdentityProvider, LookupInput, LookupOutcome, OwnedEntries,
    ProviderConfigError, ProviderEid, ProviderResponse, RequestCookies, SourceInput,
    destination_allowed, parse_sources, validate_enabled_configs,
};
use crate::ec::rate_limiter::RateLimiter;
use crate::ec::registry::PartnerRegistry;
use crate::ec::write_plan::{WritePlanInput, plan_write};
use crate::settings::IdentityProviderConfig;

/// Coordinates server-side identity lookups for one navigation.
///
/// Implementations own eligibility, consent, destination checks, the shared
/// time budget and validation. Providers never see KV, HTTP or the clock,
/// and see cookies only as [`RequestCookies`]. Implementations never write
/// KV: they return the change and the caller spends the [`crate::ec::page_load::PageLoadWrite`] permit.
#[allow(async_fn_in_trait)]
pub trait IdentityCoordinator {
    /// Starts lookups for every eligible provider without waiting.
    ///
    /// Returns empty pending lookups when no provider is enabled; skipped
    /// providers are recorded and reported by [`Self::collect`].
    async fn dispatch<T: LookupTransport>(
        &self,
        transport: &T,
        lookup: &LookupContext<'_>,
    ) -> PendingLookups<T::Pending>;

    /// Waits for dispatched lookups until all finish or the deadline passes,
    /// then parses and validates each response.
    ///
    /// Never fails: per-provider failures are recorded in [`LookupResults`].
    async fn collect<T: LookupTransport>(
        &self,
        transport: &T,
        pending: PendingLookups<T::Pending>,
    ) -> LookupResults;

    /// Merges browser ingest and lookup results into the navigation's single
    /// row change.
    ///
    /// Returns `None` when the row would not change, so no write is made.
    fn plan_write(&self, input: &WritePlanInput<'_>) -> Option<RowChange>;
}

/// Request-scoped inputs to [`IdentityCoordinator::dispatch`].
pub struct LookupContext<'a> {
    /// Active EC ID.
    pub ec_id: &'a str,
    /// Row snapshot read before dispatch.
    pub snapshot: &'a EcKvSnapshot,
    /// Request consent.
    pub consent: &'a ConsentContext,
    /// Client signals lent to providers.
    pub client: ClientSignals<'a>,
    /// Publisher domain.
    pub publisher_domain: &'a str,
    /// Request cookies, read by providers that use first-party cookies.
    pub cookies: RequestCookies<'a>,
    /// Existing EC rate limiter, used only for `rate_limit_per_hour`.
    pub rate_limiter: &'a dyn RateLimiter,
    /// Request time.
    pub now: UnixSeconds,
}

/// Lookups in flight for one navigation.
///
/// Only the coordinator module can create a non-empty value. `Default`
/// (empty) and `Debug` (counts only) are implemented by hand so they do not
/// require bounds on `Pending`.
pub struct PendingLookups<Pending> {
    requests: Vec<Tagged<Pending>>,
    deadline: Option<Deadline>,
    /// EC ID the lookups were dispatched for.
    ec_id: String,
    /// Request time, for validating `exp` and resolving relative expiry.
    now: UnixSeconds,
    /// Providers that finished at dispatch (skipped or failed to send).
    early: Vec<Tagged<LookupStatus>>,
    /// Providers whose IDs were read from first-party cookies, validated.
    from_cookies: Vec<Tagged<ProviderLookup>>,
    /// Provider state to hand back with each in-flight request's response.
    contexts: Vec<Tagged<String>>,
}

impl<Pending> Default for PendingLookups<Pending> {
    fn default() -> Self {
        Self {
            requests: Vec::new(),
            deadline: None,
            ec_id: String::new(),
            now: UnixSeconds::new(0),
            early: Vec::new(),
            from_cookies: Vec::new(),
            contexts: Vec::new(),
        }
    }
}

impl<Pending> std::fmt::Debug for PendingLookups<Pending> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingLookups")
            .field("requests", &self.requests.len())
            .field("early", &self.early.len())
            .field("from_cookies", &self.from_cookies.len())
            .finish_non_exhaustive()
    }
}

impl<Pending> PendingLookups<Pending> {
    /// Whether nothing was dispatched or skipped.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty() && self.early.is_empty() && self.from_cookies.is_empty()
    }
}

/// A configured provider handed to [`ProviderCoordinator::new`].
pub struct RegisteredProvider<P> {
    /// The provider's configuration entry.
    pub config: IdentityProviderConfig,
    /// The built provider.
    pub provider: P,
    rate_limit_per_hour: Option<NonZeroU32>,
}

impl<P> RegisteredProvider<P> {
    /// Pairs a configuration entry with its built provider.
    #[must_use]
    pub fn new(config: IdentityProviderConfig, provider: P) -> Self {
        Self {
            config,
            provider,
            rate_limit_per_hour: None,
        }
    }

    /// Applies an hourly per-EC lookup limit (carried over from pull sync).
    #[must_use]
    pub fn with_rate_limit(mut self, per_hour: NonZeroU32) -> Self {
        self.rate_limit_per_hour = Some(per_hour);
        self
    }
}

/// A provider with the operator policy the coordinator applies to it.
struct Registered<P> {
    id: ProviderId,
    policy: ProviderPolicy,
    provider: P,
}

/// Operator policy for one provider, validated when the coordinator is built.
struct ProviderPolicy {
    sources: Box<[SourceDomain]>,
    allowed_domains: Box<[Box<str>]>,
    tcf_vendor_id: Option<NonZeroU16>,
    rate_limit_per_hour: Option<NonZeroU32>,
}

/// The production coordinator, built per request from settings.
///
/// Generic over the provider type: production uses the boxed default, tests
/// and single-provider builds use a concrete type.
pub struct ProviderCoordinator<P = Box<dyn IdentityProvider>> {
    providers: Box<[Registered<P>]>,
    /// Partner config: `openrtb_atype` for validation.
    partners: Arc<PartnerRegistry>,
    budget: Duration,
}

impl<P: IdentityProvider> ProviderCoordinator<P> {
    /// Validates and registers the enabled providers.
    ///
    /// Disabled providers are dropped: they make no requests.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderConfigError::Invalid`] when an enabled provider's
    /// endpoint is not `https` with an allowlisted host, a source has no
    /// partner entry, a source is outside the provider's capabilities, a
    /// source is claimed by two enabled providers, or more than `u16::MAX`
    /// providers are enabled.
    pub fn new(
        registered: Vec<RegisteredProvider<P>>,
        partners: Arc<PartnerRegistry>,
        budget: Duration,
    ) -> Result<Self, Report<ProviderConfigError>> {
        let enabled: Vec<RegisteredProvider<P>> = registered
            .into_iter()
            .filter(|registered| registered.config.enabled)
            .collect();
        if u16::try_from(enabled.len()).is_err() {
            return Err(Report::new(ProviderConfigError::Invalid)
                .attach("too many identity providers are enabled"));
        }
        validate_enabled_configs(
            enabled.iter().map(|registered| &registered.config),
            &partners,
        )?;

        let providers = enabled
            .into_iter()
            .map(Self::register)
            .collect::<Result<Box<[_]>, Report<ProviderConfigError>>>()?;

        Ok(Self {
            providers,
            partners,
            budget,
        })
    }

    fn register(
        registered: RegisteredProvider<P>,
    ) -> Result<Registered<P>, Report<ProviderConfigError>> {
        let RegisteredProvider {
            config,
            provider,
            rate_limit_per_hour,
        } = registered;
        let sources = parse_sources(&config)?;
        let capable: HashSet<&SourceDomain> = provider
            .capabilities()
            .sources
            .iter()
            .map(|capability| &capability.source)
            .collect();
        if let Some(source) = sources.iter().find(|source| !capable.contains(source)) {
            return Err(Report::new(ProviderConfigError::Invalid).attach(format!(
                "source `{source}` of provider `{}` is outside its kind's capabilities",
                config.id
            )));
        }
        Ok(Registered {
            id: ProviderId::new(&config.id),
            policy: ProviderPolicy {
                sources,
                allowed_domains: config
                    .allowed_domains
                    .iter()
                    .map(|domain| Box::from(domain.as_str()))
                    .collect(),
                tcf_vendor_id: config.tcf_vendor_id,
                rate_limit_per_hour,
            },
            provider,
        })
    }

    /// Whether no provider is enabled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Claimed sources lacking a live provider-written UID (a browser
    /// fallback does not count), minus sources needing input V1
    /// never has; the flag reports whether any source was removed for input.
    fn due_sources(
        registered: &Registered<P>,
        entry: Option<&KvEntry>,
        now: UnixSeconds,
    ) -> (Vec<SourceDomain>, bool) {
        let capabilities = registered.provider.capabilities();
        let mut input_blocked = false;
        let mut due = Vec::new();
        for source in &registered.policy.sources {
            let record = entry
                .and_then(|entry| entry.ids.get(source.as_str()))
                .filter(|record| record.is_provider_written());
            // A UID whose provider asked for a refresh makes its source due;
            // the stored refresh state is the lookup's input.
            if record.is_some_and(|record| record.refresh_due_for(&registered.id, now)) {
                due.push(source.clone());
                continue;
            }
            if record.is_some_and(|record| record.has_live_uid(now)) {
                continue;
            }
            let needs_input = capabilities
                .sources
                .iter()
                .find(|capability| capability.source == *source)
                .is_some_and(|capability| capability.input != SourceInput::Anonymous);
            if needs_input {
                input_blocked = true;
                continue;
            }
            due.push(source.clone());
        }
        (due, input_blocked)
    }

    /// Whether the provider's hourly per-EC limit is reached. Limiter errors
    /// fail open, matching pull sync.
    fn rate_limited(registered: &Registered<P>, lookup: &LookupContext<'_>) -> bool {
        let Some(limit) = registered.policy.rate_limit_per_hour else {
            return false;
        };
        let hash = lookup.ec_id.split('.').next().unwrap_or(lookup.ec_id);
        let key = format!("identity:{}:{hash}", registered.id);
        match lookup.rate_limiter.exceeded(&key, limit.get()) {
            Ok(exceeded) => exceeded,
            Err(report) => {
                log::warn!(
                    "Identity lookup: provider={} rate limiter failed, allowing lookup: {report:?}",
                    registered.id
                );
                false
            }
        }
    }

    fn lookup(
        &self,
        provider: ProviderIndex,
        status: LookupStatus,
        entries: ValidatedEntries,
    ) -> ProviderLookup {
        let registered = &self.providers[provider.get()];
        let cleared = if status == LookupStatus::Skipped(SkipReason::OptedOut) {
            registered.policy.sources.to_vec()
        } else {
            Vec::new()
        };
        ProviderLookup {
            provider: registered.id.clone(),
            status,
            entries,
            cleared,
        }
    }

    fn interpret(
        &self,
        provider: ProviderIndex,
        response: Result<TransportResponse, Report<TransportError>>,
        context: Option<&str>,
        now: UnixSeconds,
    ) -> ProviderLookup {
        let registered = &self.providers[provider.get()];
        let response = match response {
            Ok(response) => response,
            Err(report) => {
                let status = match report.current_context() {
                    TransportError::Timeout => LookupStatus::Timeout,
                    TransportError::Send | TransportError::BodyTooLarge => {
                        log::debug!(
                            "Identity lookup: provider={} transport failed: {report:?}",
                            registered.id
                        );
                        LookupStatus::TransportFailed
                    }
                };
                return self.lookup(provider, status, Vec::new());
            }
        };
        if matches!(
            response.status,
            http::StatusCode::NOT_FOUND | http::StatusCode::NO_CONTENT
        ) {
            return self.lookup(provider, LookupStatus::NoMatch, Vec::new());
        }
        if !response.status.is_success() {
            return self.lookup(
                provider,
                LookupStatus::HttpError(response.status),
                Vec::new(),
            );
        }
        let parsed = registered.provider.parse_response(ProviderResponse {
            status: response.status,
            headers: &response.headers,
            body: &response.body,
            context,
        });
        let eids = match parsed {
            Ok(LookupOutcome::Found(eids)) => eids,
            Ok(LookupOutcome::NoMatch) => {
                return self.lookup(provider, LookupStatus::NoMatch, Vec::new());
            }
            Ok(LookupOutcome::OptedOut) => {
                return self.lookup(
                    provider,
                    LookupStatus::Skipped(SkipReason::OptedOut),
                    Vec::new(),
                );
            }
            Err(report) => {
                log::debug!(
                    "Identity lookup: provider={} response not parsed: {}",
                    registered.id,
                    report.current_context()
                );
                return self.lookup(provider, LookupStatus::InvalidResponse, Vec::new());
            }
        };
        self.validated(provider, eids, now)
    }

    /// Validates parsed entries into the provider's outcome.
    fn validated(
        &self,
        provider: ProviderIndex,
        eids: Vec<ProviderEid<'_>>,
        now: UnixSeconds,
    ) -> ProviderLookup {
        let registered = &self.providers[provider.get()];
        let policy = ValidationPolicy {
            provider: &registered.id,
            claimed: &registered.policy.sources,
            shape: registered.provider.capabilities().shape,
            partners: &self.partners,
            now,
        };
        let (entries, dropped) = validate(eids, &policy);
        let kept = entries
            .iter()
            .flat_map(|(_, eids)| eids)
            .map(|eid| eid.uids().len())
            .sum::<usize>();
        if kept == 0 {
            log::info!(
                "Identity lookup: provider={} dropped every UID ({} drops)",
                registered.id,
                dropped.total()
            );
            return self.lookup(provider, LookupStatus::NoMatch, Vec::new());
        }
        let kept = u16::try_from(kept).unwrap_or(u16::MAX);
        self.lookup(provider, LookupStatus::Found { kept, dropped }, entries)
    }
}

impl<P: IdentityProvider> IdentityCoordinator for ProviderCoordinator<P> {
    async fn dispatch<T: LookupTransport>(
        &self,
        transport: &T,
        lookup: &LookupContext<'_>,
    ) -> PendingLookups<T::Pending> {
        let mut pending = PendingLookups {
            ec_id: lookup.ec_id.to_owned(),
            now: lookup.now,
            ..PendingLookups::default()
        };
        if self.providers.is_empty() {
            return pending;
        }
        let entry = lookup.snapshot.entry_for(lookup.ec_id);
        // Missing EC or EID consent, a withdrawal tombstone, or an unreadable
        // row (which may hide a tombstone) skips every provider: a lookup
        // discloses the EC ID before finalize could decline it.
        let readable = !matches!(lookup.snapshot, EcKvSnapshot::Failed { .. });
        let consented = readable
            && ec_consent_granted(lookup.consent)
            && allows_eid_persistence(lookup.consent)
            && entry.is_none_or(|entry| entry.consent.ok);
        let deadline = Deadline::after(self.budget);
        pending.deadline = Some(deadline);
        for (index, registered) in self.providers.iter().enumerate() {
            let provider = ProviderIndex::new(index);
            let skip = |reason| Tagged {
                provider,
                value: LookupStatus::Skipped(reason),
            };
            let vendor_denied = registered
                .policy
                .tcf_vendor_id
                .is_some_and(|vendor| tcf_vendor_consent_denied(lookup.consent, vendor));
            let region_excluded =
                lookup.consent.gdpr_applies && !registered.provider.runs_where_gdpr_applies();
            if !consented || vendor_denied || region_excluded {
                pending.early.push(skip(SkipReason::Consent));
                continue;
            }
            // A remembered opt-out keeps the provider from being called until
            // it lapses.
            if entry.is_some_and(|entry| entry.opted_out(&registered.id, lookup.now)) {
                pending.early.push(skip(SkipReason::OptedOut));
                continue;
            }
            let (mut due, input_blocked) = Self::due_sources(registered, entry, lookup.now);
            // The vendor's own first-party cookies are read on every
            // navigation, so an opt-out or a changed ID is noticed while the
            // sources still hold live UIDs. Their IDs stand in for a lookup.
            match registered.provider.read_request_cookies(lookup.cookies) {
                CookieRead::OptedOut => {
                    log::debug!(
                        "Identity lookup: provider={} opted out in its cookies",
                        registered.id
                    );
                    pending.early.push(skip(SkipReason::OptedOut));
                    continue;
                }
                CookieRead::Found(eids) => {
                    let found = self.validated(provider, eids, lookup.now);
                    if matches!(found.status, LookupStatus::Found { .. }) {
                        // Only the sources the cookies filled are spared a
                        // lookup; the provider's other due sources are
                        // still looked up.
                        due.retain(|source| {
                            found.entries.iter().all(|(filled, _)| filled != source)
                        });
                        // A cookie repeating the held UIDs is not rewritten,
                        // so its relative expiry does not cause a write on
                        // every navigation.
                        if due.is_empty() && holds_same_uids(entry, &found, lookup.now) {
                            pending.early.push(skip(SkipReason::NotDue));
                            continue;
                        }
                        pending.from_cookies.push(Tagged {
                            provider,
                            value: found,
                        });
                        if due.is_empty() {
                            continue;
                        }
                    }
                }
                CookieRead::Absent => {}
            }
            if due.is_empty() {
                pending.early.push(skip(if input_blocked {
                    SkipReason::Input
                } else {
                    SkipReason::NotDue
                }));
                continue;
            }
            let input = LookupInput {
                ec_id: lookup.ec_id,
                consent: lookup.consent,
                client: lookup.client,
                publisher_domain: lookup.publisher_domain,
                due_sources: &due,
                current: OwnedEntries::new(entry, &registered.policy.sources, &registered.id),
                cookies: lookup.cookies,
                hashed_email: None,
            };
            if let Some(eids) = registered.provider.generate(&input) {
                pending.from_cookies.push(Tagged {
                    provider,
                    value: self.validated(provider, eids, lookup.now),
                });
                continue;
            }
            // The request is built before the rate limit is checked, so a
            // provider that makes no lookup spends none of its budget.
            let mut request = match registered.provider.build_request(&input) {
                Ok(request) => request,
                Err(report) => {
                    log::debug!(
                        "Identity lookup: provider={} request not built: {}",
                        registered.id,
                        report.current_context()
                    );
                    pending.early.push(skip(SkipReason::Input));
                    continue;
                }
            };
            if Self::rate_limited(registered, lookup) {
                pending.early.push(skip(SkipReason::RateLimited));
                continue;
            }
            if !destination_allowed(&request.url, &registered.policy.allowed_domains) {
                pending.early.push(skip(SkipReason::Destination));
                continue;
            }
            let context = request.context.take();
            match transport.start(request, deadline).await {
                Ok(value) => {
                    pending.requests.push(Tagged { provider, value });
                    if let Some(value) = context {
                        pending.contexts.push(Tagged { provider, value });
                    }
                }
                Err(report) => {
                    log::warn!(
                        "Identity lookup: provider={} send failed: {}",
                        registered.id,
                        report.current_context()
                    );
                    pending.early.push(Tagged {
                        provider,
                        value: LookupStatus::TransportFailed,
                    });
                }
            }
        }
        pending
    }

    async fn collect<T: LookupTransport>(
        &self,
        transport: &T,
        pending: PendingLookups<T::Pending>,
    ) -> LookupResults {
        let PendingLookups {
            requests,
            deadline,
            ec_id,
            now,
            early,
            from_cookies,
            contexts,
        } = pending;
        let responses = match deadline {
            Some(deadline) if !requests.is_empty() => transport.gather(requests, deadline).await,
            _ => Vec::new(),
        };
        let mut slots: Vec<Option<ProviderLookup>> =
            (0..self.providers.len()).map(|_| None).collect();
        for Tagged { provider, value } in early {
            slots[provider.get()] = Some(self.lookup(provider, value, Vec::new()));
        }
        // A provider's cookie result replaces its early skip, and its later
        // lookup or generated result is merged into the cookie result.
        let mut filled: Vec<Option<ProviderLookup>> =
            (0..self.providers.len()).map(|_| None).collect();
        for Tagged { provider, value } in from_cookies {
            let slot = &mut filled[provider.get()];
            *slot = Some(match slot.take() {
                Some(earlier) => merge_lookups(earlier, value),
                None => value,
            });
        }
        for Tagged { provider, value } in responses {
            let context = contexts
                .iter()
                .find(|tagged| tagged.provider == provider)
                .map(|tagged| tagged.value.as_str());
            let later = self.interpret(provider, value, context, now);
            let slot = &mut filled[provider.get()];
            *slot = Some(match slot.take() {
                Some(earlier) => merge_lookups(earlier, later),
                None => later,
            });
        }
        for (slot, value) in slots.iter_mut().zip(filled) {
            if value.is_some() {
                *slot = value;
            }
        }
        let lookups: Vec<ProviderLookup> = slots.into_iter().flatten().collect();
        for lookup in &lookups {
            log_outcome(lookup);
        }
        LookupResults::new(&ec_id, lookups)
    }

    fn plan_write(&self, input: &WritePlanInput<'_>) -> Option<RowChange> {
        plan_write(input)
    }
}

/// Merges a provider's cookie result with its later lookup or generated
/// result for the sources the cookies did not fill.
///
/// An opt-out wins; otherwise the found entries of both are kept, and a
/// later result that found nothing leaves the cookie result.
fn merge_lookups(earlier: ProviderLookup, later: ProviderLookup) -> ProviderLookup {
    let LookupStatus::Found { dropped, .. } = later.status else {
        return if later.status == LookupStatus::Skipped(SkipReason::OptedOut)
            || !matches!(earlier.status, LookupStatus::Found { .. })
        {
            later
        } else {
            earlier
        };
    };
    if !matches!(earlier.status, LookupStatus::Found { .. }) {
        return later;
    }
    let mut entries = later.entries;
    for (source, eids) in earlier.entries {
        if entries.iter().all(|(held, _)| *held != source) {
            entries.push((source, eids));
        }
    }
    let merged = entries
        .iter()
        .flat_map(|(_, eids)| eids)
        .map(|eid| eid.uids().len())
        .sum::<usize>();
    ProviderLookup {
        status: LookupStatus::Found {
            kept: u16::try_from(merged).unwrap_or(u16::MAX),
            dropped,
        },
        entries,
        ..later
    }
}

/// Whether the row already holds exactly the UIDs `found` read from cookies,
/// for every source `found` returned.
fn holds_same_uids(entry: Option<&KvEntry>, found: &ProviderLookup, now: UnixSeconds) -> bool {
    let Some(entry) = entry else {
        return false;
    };
    found.entries.iter().all(|(source, eids)| {
        let Some(record) = entry.ids.get(source.as_str()) else {
            return false;
        };
        let held: HashSet<&str> = record
            .eids()
            .iter()
            .flat_map(KvEid::uids)
            .filter(|uid| uid.is_live(now))
            .map(|uid| uid.id().as_str())
            .collect();
        let read: HashSet<&str> = eids
            .iter()
            .flat_map(KvEid::uids)
            .map(|uid| uid.id().as_str())
            .collect();
        held == read
    })
}

fn log_outcome(lookup: &ProviderLookup) {
    let (kept, dropped) = match lookup.status {
        LookupStatus::Found { kept, dropped } => (kept, dropped.total()),
        _ => (0, 0),
    };
    if let LookupStatus::HttpError(status) = lookup.status {
        log::info!(
            "Identity lookup: provider={} outcome={} status={status}",
            lookup.provider,
            lookup.status.metric_name()
        );
        return;
    }
    log::info!(
        "Identity lookup: provider={} outcome={} kept={kept} dropped={dropped}",
        lookup.provider,
        lookup.status.metric_name()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::TcfConsent;
    use crate::ec::eid_types::{AgentType, EidFields, Expiry, UidValue};
    use crate::ec::kv::KvIdentityGraph;
    use crate::ec::kv_types::{KvEid, KvPartnerId, KvRefresh, KvUid};
    use crate::ec::provider::{
        EidLevelFields, EidShape, ProviderCapabilities, ProviderEid, ProviderError, ProviderExpiry,
        ProviderRequest, ProviderUid, SourceCapability,
    };
    use crate::error::TrustedServerError;
    use crate::settings::EcPartner;
    use std::borrow::Cow;
    use std::cell::{Cell, RefCell};
    use std::num::NonZeroUsize;

    const EC_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123";

    type Reply = fn() -> Result<TransportResponse, Report<TransportError>>;

    /// Context the echo provider sets on its request and expects back.
    const ECHO_CONTEXT: &str = "echo-context";

    /// Echoes the response body as one UID; when the cookie name is set, also
    /// reads that cookie as one UID.
    struct EchoProvider(ProviderCapabilities, Option<&'static str>);

    impl IdentityProvider for EchoProvider {
        fn capabilities(&self) -> &ProviderCapabilities {
            &self.0
        }

        fn build_request(
            &self,
            lookup: &LookupInput<'_>,
        ) -> Result<ProviderRequest, Report<ProviderError>> {
            Ok(ProviderRequest {
                method: http::Method::GET,
                url: url::Url::parse(&format!(
                    "https://ids.example.com/lookup?due={}",
                    lookup.due_sources.len()
                ))
                .expect("should parse"),
                headers: http::HeaderMap::new(),
                body: bytes::Bytes::new(),
                context: Some(ECHO_CONTEXT.to_owned()),
            })
        }

        fn parse_response<'body>(
            &self,
            response: ProviderResponse<'body>,
        ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
            if response.context != Some(ECHO_CONTEXT) {
                return Err(Report::new(ProviderError::InvalidResponse));
            }
            let id = std::str::from_utf8(response.body)
                .map_err(|_| Report::new(ProviderError::InvalidResponse))?;
            if id == "optout" {
                return Ok(LookupOutcome::OptedOut);
            }
            Ok(LookupOutcome::Found(vec![ProviderEid {
                source: Cow::Borrowed("ids.example.com"),
                inserter: None,
                matcher: None,
                mm: None,
                ext: None,
                uids: vec![ProviderUid {
                    id: Cow::Borrowed(id),
                    atype: None,
                    ext: None,
                    expiry: ProviderExpiry::Unspecified,
                    refresh: None,
                }],
            }]))
        }

        fn read_request_cookies<'cookie>(
            &self,
            cookies: RequestCookies<'cookie>,
        ) -> CookieRead<'cookie> {
            let Some(id) = self.1.and_then(|name| cookies.get(name)) else {
                return CookieRead::Absent;
            };
            if id == "optout" {
                return CookieRead::OptedOut;
            }
            CookieRead::Found(vec![ProviderEid {
                source: Cow::Borrowed("ids.example.com"),
                inserter: None,
                matcher: None,
                mm: None,
                ext: None,
                uids: vec![ProviderUid {
                    id: Cow::Borrowed(id),
                    atype: None,
                    ext: None,
                    expiry: ProviderExpiry::At(UnixSeconds::new(5000)),
                    refresh: None,
                }],
            }])
        }
    }

    /// Answers every request with a canned result, recording the URLs sent.
    struct CannedTransport {
        reply: Reply,
        sent: RefCell<Vec<String>>,
    }

    impl CannedTransport {
        fn new(reply: Reply) -> Self {
            Self {
                reply,
                sent: RefCell::default(),
            }
        }
    }

    impl LookupTransport for CannedTransport {
        type Pending = ();

        async fn start(
            &self,
            request: ProviderRequest,
            _deadline: Deadline,
        ) -> Result<(), Report<TransportError>> {
            self.sent.borrow_mut().push(request.url.to_string());
            Ok(())
        }

        async fn gather(
            &self,
            pending: Vec<Tagged<()>>,
            _deadline: Deadline,
        ) -> Vec<Tagged<Result<TransportResponse, Report<TransportError>>>> {
            pending
                .into_iter()
                .map(|tagged| Tagged {
                    provider: tagged.provider,
                    value: (self.reply)(),
                })
                .collect()
        }
    }

    struct Limiter(bool);

    impl RateLimiter for Limiter {
        fn exceeded(
            &self,
            _key: &str,
            _hourly_limit: u32,
        ) -> Result<bool, Report<TrustedServerError>> {
            Ok(self.0)
        }
    }

    fn reply(status: http::StatusCode, body: &'static [u8]) -> TransportResponse {
        TransportResponse {
            status,
            headers: http::HeaderMap::new(),
            body: bytes::Bytes::from_static(body),
        }
    }

    fn ok_reply() -> Result<TransportResponse, Report<TransportError>> {
        Ok(reply(http::StatusCode::OK, b"p-1"))
    }

    fn timeout_reply() -> Result<TransportResponse, Report<TransportError>> {
        Err(Report::new(TransportError::Timeout))
    }

    fn not_found_reply() -> Result<TransportResponse, Report<TransportError>> {
        Ok(reply(http::StatusCode::NOT_FOUND, b""))
    }

    fn server_error_reply() -> Result<TransportResponse, Report<TransportError>> {
        Ok(reply(http::StatusCode::BAD_GATEWAY, b""))
    }

    fn invalid_reply() -> Result<TransportResponse, Report<TransportError>> {
        Ok(reply(http::StatusCode::OK, b"\xff"))
    }

    fn partners() -> Arc<PartnerRegistry> {
        let partner: EcPartner = serde_json::from_value(serde_json::json!({
            "name": "ids", "source_domain": "ids.example.com", "bidstream_enabled": true
        }))
        .expect("should build partner");
        Arc::new(PartnerRegistry::from_config(&[partner]).expect("should build registry"))
    }

    fn provider_config(extra: &str) -> IdentityProviderConfig {
        toml::from_str(&format!(
            "id = \"example-provider\"\nkind = \"echo\"\nendpoint = \"https://ids.example.com/lookup\"\nallowed_domains = [\"ids.example.com\"]\nsources = [\"ids.example.com\"]\n{extra}"
        ))
        .expect("should parse config")
    }

    fn echo_provider(input: SourceInput) -> EchoProvider {
        cookie_provider(input, None)
    }

    fn cookie_provider(input: SourceInput, cookie: Option<&'static str>) -> EchoProvider {
        EchoProvider(
            ProviderCapabilities {
                sources: Box::new([SourceCapability {
                    source: SourceDomain::try_from("ids.example.com").expect("should parse"),
                    input,
                }]),
                shape: EidShape {
                    max_eids: NonZeroUsize::new(4).expect("should be non-zero"),
                    max_uids_per_eid: NonZeroUsize::new(4).expect("should be non-zero"),
                    eid_level_fields: EidLevelFields::Preserved,
                },
            },
            cookie,
        )
    }

    fn coordinator_with(
        registered: RegisteredProvider<EchoProvider>,
    ) -> ProviderCoordinator<EchoProvider> {
        ProviderCoordinator::new(vec![registered], partners(), Duration::from_millis(200))
            .expect("should build coordinator")
    }

    fn coordinator(extra: &str) -> ProviderCoordinator<EchoProvider> {
        coordinator_with(RegisteredProvider::new(
            provider_config(extra),
            echo_provider(SourceInput::Anonymous),
        ))
    }

    fn granting() -> ConsentContext {
        ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            ..Default::default()
        }
    }

    fn missing() -> EcKvSnapshot {
        EcKvSnapshot::Missing {
            ec_id: EC_ID.to_owned(),
        }
    }

    fn run_with<P: IdentityProvider>(
        coordinator: &ProviderCoordinator<P>,
        transport: &CannedTransport,
        snapshot: &EcKvSnapshot,
        consent: &ConsentContext,
        limiter: &dyn RateLimiter,
    ) -> LookupResults {
        run_with_cookies(
            coordinator,
            transport,
            snapshot,
            consent,
            limiter,
            "vendor_id=c-1",
        )
    }

    fn run_with_cookies<P: IdentityProvider>(
        coordinator: &ProviderCoordinator<P>,
        transport: &CannedTransport,
        snapshot: &EcKvSnapshot,
        consent: &ConsentContext,
        limiter: &dyn RateLimiter,
        cookies: &str,
    ) -> LookupResults {
        let context = LookupContext {
            ec_id: EC_ID,
            snapshot,
            consent,
            client: ClientSignals {
                ip: None,
                user_agent: None,
            },
            publisher_domain: "www.example.com",
            cookies: RequestCookies::new(cookies),
            rate_limiter: limiter,
            now: UnixSeconds::new(1000),
        };
        futures::executor::block_on(async {
            let pending = coordinator.dispatch(transport, &context).await;
            coordinator.collect(transport, pending).await
        })
    }

    fn run<P: IdentityProvider>(
        coordinator: &ProviderCoordinator<P>,
        transport: &CannedTransport,
        snapshot: &EcKvSnapshot,
        consent: &ConsentContext,
    ) -> LookupResults {
        run_with(coordinator, transport, snapshot, consent, &Limiter(false))
    }

    fn only_status(results: &LookupResults) -> LookupStatus {
        results
            .iter()
            .next()
            .expect("should report the provider")
            .status
    }

    fn seeded_snapshot(entry: &KvEntry) -> EcKvSnapshot {
        let graph = KvIdentityGraph::in_memory("coord");
        graph.create(EC_ID, entry).expect("should seed");
        graph.load_snapshot(EC_ID)
    }

    #[test]
    fn due_source_is_looked_up_and_validated() {
        let coordinator = coordinator("");
        let transport = CannedTransport::new(ok_reply);

        let results = run(&coordinator, &transport, &missing(), &granting());

        let lookup = results.iter().next().expect("should report the provider");
        assert!(
            matches!(lookup.status, LookupStatus::Found { kept: 1, .. }),
            "should keep one UID"
        );
        assert_eq!(
            lookup.entries[0].1[0].uids()[0]
                .owner()
                .map(ProviderId::as_str),
            Some("example-provider"),
            "should record the owner"
        );
        assert_eq!(transport.sent.borrow().len(), 1, "should send one request");
    }

    fn provider_record(id: &str) -> KvPartnerId {
        let uid = KvUid::new(
            UidValue::try_from(id).expect("should build uid"),
            AgentType::try_from(1).expect("should build atype"),
            None,
            Some(ProviderId::new("example-provider")),
            Expiry::Never,
        );
        KvPartnerId::from_eids(
            vec![KvEid::new(EidFields::default(), vec![uid])],
            UnixSeconds::new(1000),
        )
        .expect("should build provider record")
    }

    #[test]
    fn source_with_browser_fallback_uid_is_still_due() {
        // Arrange
        let coordinator = coordinator("");
        let transport = CannedTransport::new(ok_reply);
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry.ids.insert(
            "ids.example.com".to_owned(),
            KvPartnerId::single("fallback"),
        );

        // Act
        let results = run(
            &coordinator,
            &transport,
            &seeded_snapshot(&entry),
            &granting(),
        );

        // Assert
        assert!(
            matches!(only_status(&results), LookupStatus::Found { .. }),
            "should look up a source holding only a browser fallback"
        );
        assert_eq!(transport.sent.borrow().len(), 1, "should send one request");
    }

    #[test]
    fn source_with_live_uid_is_not_due() {
        let coordinator = coordinator("");
        let transport = CannedTransport::new(ok_reply);
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry
            .ids
            .insert("ids.example.com".to_owned(), provider_record("p-1"));

        let results = run(
            &coordinator,
            &transport,
            &seeded_snapshot(&entry),
            &granting(),
        );

        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::NotDue),
            "should skip"
        );
        assert!(transport.sent.borrow().is_empty(), "should send nothing");
    }

    #[test]
    fn tombstoned_row_skips_for_consent() {
        let coordinator = coordinator("");
        let transport = CannedTransport::new(ok_reply);

        let results = run(
            &coordinator,
            &transport,
            &seeded_snapshot(&KvEntry::tombstone(1000)),
            &granting(),
        );

        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::Consent),
            "should skip a tombstoned row"
        );
        assert!(transport.sent.borrow().is_empty(), "should send nothing");
    }

    #[test]
    fn missing_ec_consent_skips_every_provider() {
        let coordinator = coordinator("");
        let cases = [
            (
                "GPC opt-out in a US state",
                ConsentContext {
                    jurisdiction: Jurisdiction::UsState("CA".to_owned()),
                    gpc: true,
                    ..Default::default()
                },
            ),
            (
                "unknown jurisdiction",
                ConsentContext {
                    jurisdiction: Jurisdiction::Unknown,
                    ..Default::default()
                },
            ),
            (
                "GDPR without a TC string",
                ConsentContext {
                    jurisdiction: Jurisdiction::Gdpr,
                    ..Default::default()
                },
            ),
        ];

        for (label, consent) in cases {
            let transport = CannedTransport::new(ok_reply);
            let mut entry = KvEntry::tombstone(1000);
            entry.consent.ok = true;

            let results = run(&coordinator, &transport, &seeded_snapshot(&entry), &consent);

            assert_eq!(
                only_status(&results),
                LookupStatus::Skipped(SkipReason::Consent),
                "should skip for {label}"
            );
            assert!(
                transport.sent.borrow().is_empty(),
                "should send nothing for {label}"
            );
        }
    }

    #[test]
    fn failed_snapshot_skips_every_provider() {
        let coordinator = coordinator("");
        let transport = CannedTransport::new(ok_reply);
        let snapshot = EcKvSnapshot::Failed {
            ec_id: EC_ID.to_owned(),
        };

        let results = run(&coordinator, &transport, &snapshot, &granting());

        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::Consent),
            "should skip when the row could not be read"
        );
        assert!(transport.sent.borrow().is_empty(), "should send nothing");
    }

    #[test]
    fn tcf_vendor_id_does_not_gate_traffic_without_tcf() {
        let coordinator = coordinator("tcf_vendor_id = 9999");
        let transport = CannedTransport::new(ok_reply);

        let results = run(&coordinator, &transport, &missing(), &granting());

        assert!(
            !matches!(only_status(&results), LookupStatus::Skipped(_)),
            "should look up when no TCF string applies outside GDPR"
        );
        assert_eq!(transport.sent.borrow().len(), 1, "should send one request");
    }

    fn tcf_consent(vendor_consents: Vec<u16>) -> ConsentContext {
        let mut purposes = vec![false; 24];
        purposes[0] = true;
        purposes[3] = true;
        ConsentContext {
            jurisdiction: Jurisdiction::Gdpr,
            gdpr_applies: true,
            tcf: Some(TcfConsent {
                version: 2,
                cmp_id: 1,
                cmp_version: 1,
                consent_screen: 0,
                consent_language: "EN".to_owned(),
                vendor_list_version: 1,
                tcf_policy_version: 4,
                created_ds: 0,
                last_updated_ds: 0,
                purpose_consents: purposes,
                purpose_legitimate_interests: vec![false; 24],
                vendor_consents,
                vendor_legitimate_interests: Vec::new(),
                special_feature_opt_ins: vec![false; 12],
            }),
            ..ConsentContext::default()
        }
    }

    #[test]
    fn missing_tcf_vendor_consent_skips_before_building_a_request() {
        let coordinator = coordinator("tcf_vendor_id = 9999");
        let transport = CannedTransport::new(ok_reply);

        let results = run(&coordinator, &transport, &missing(), &tcf_consent(vec![1]));

        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::Consent),
            "should skip"
        );
        assert!(transport.sent.borrow().is_empty(), "should send nothing");
    }

    #[test]
    fn granted_tcf_vendor_consent_allows_the_lookup() {
        let coordinator = coordinator("tcf_vendor_id = 9999");
        let transport = CannedTransport::new(ok_reply);

        let results = run(
            &coordinator,
            &transport,
            &missing(),
            &tcf_consent(vec![9999]),
        );

        assert!(
            matches!(only_status(&results), LookupStatus::Found { .. }),
            "should look up with vendor consent"
        );
    }

    /// An echo provider whose IDs are not offered where GDPR applies.
    struct OutsideGdprProvider(EchoProvider);

    impl IdentityProvider for OutsideGdprProvider {
        fn capabilities(&self) -> &ProviderCapabilities {
            self.0.capabilities()
        }

        fn build_request(
            &self,
            lookup: &LookupInput<'_>,
        ) -> Result<ProviderRequest, Report<ProviderError>> {
            self.0.build_request(lookup)
        }

        fn parse_response<'body>(
            &self,
            response: ProviderResponse<'body>,
        ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
            self.0.parse_response(response)
        }

        fn read_request_cookies<'cookie>(
            &self,
            cookies: RequestCookies<'cookie>,
        ) -> CookieRead<'cookie> {
            self.0.read_request_cookies(cookies)
        }

        fn runs_where_gdpr_applies(&self) -> bool {
            false
        }
    }

    #[test]
    fn a_provider_outside_gdpr_regions_is_skipped_where_gdpr_applies() {
        let coordinator = ProviderCoordinator::new(
            vec![RegisteredProvider::new(
                provider_config(""),
                OutsideGdprProvider(cookie_provider(SourceInput::Anonymous, Some("vendor_id"))),
            )],
            partners(),
            Duration::from_millis(200),
        )
        .expect("should build coordinator");
        let transport = CannedTransport::new(ok_reply);

        let gdpr = run(&coordinator, &transport, &missing(), &tcf_consent(vec![]));
        let outside = run(&coordinator, &transport, &missing(), &granting());

        assert_eq!(
            only_status(&gdpr),
            LookupStatus::Skipped(SkipReason::Consent),
            "should neither read cookies nor look up where GDPR applies"
        );
        assert!(
            matches!(only_status(&outside), LookupStatus::Found { .. }),
            "should read the cookie where GDPR does not apply"
        );
    }

    #[test]
    fn hashed_email_sources_are_skipped_for_input() {
        let coordinator = coordinator_with(RegisteredProvider::new(
            provider_config(""),
            echo_provider(SourceInput::HashedEmail),
        ));
        let transport = CannedTransport::new(ok_reply);

        let results = run(&coordinator, &transport, &missing(), &granting());

        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::Input),
            "should skip sources needing hashed email"
        );
    }

    #[test]
    fn rate_limited_provider_is_skipped() {
        let coordinator = coordinator_with(
            RegisteredProvider::new(provider_config(""), echo_provider(SourceInput::Anonymous))
                .with_rate_limit(NonZeroU32::new(1).expect("should be non-zero")),
        );
        let transport = CannedTransport::new(ok_reply);

        let results = run_with(
            &coordinator,
            &transport,
            &missing(),
            &granting(),
            &Limiter(true),
        );

        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::RateLimited),
            "should skip a rate-limited provider"
        );
        assert!(transport.sent.borrow().is_empty(), "should send nothing");
    }

    #[test]
    fn built_url_outside_allowlist_is_rejected() {
        let coordinator = coordinator("");
        // The echo provider builds its URL on the configured host; narrow the
        // policy afterwards so the built URL falls outside the allowlist.
        let coordinator = ProviderCoordinator {
            providers: coordinator
                .providers
                .into_vec()
                .into_iter()
                .map(|mut registered| {
                    registered.policy.allowed_domains = Box::new([Box::from("other.example.com")]);
                    registered
                })
                .collect(),
            ..coordinator
        };
        let transport = CannedTransport::new(ok_reply);

        let results = run(&coordinator, &transport, &missing(), &granting());

        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::Destination),
            "should reject the destination"
        );
        assert!(transport.sent.borrow().is_empty(), "should send nothing");
    }

    #[test]
    fn transport_and_http_outcomes_map_to_statuses() {
        let cases: [(Reply, LookupStatus); 4] = [
            (timeout_reply, LookupStatus::Timeout),
            (not_found_reply, LookupStatus::NoMatch),
            (
                server_error_reply,
                LookupStatus::HttpError(http::StatusCode::BAD_GATEWAY),
            ),
            (invalid_reply, LookupStatus::InvalidResponse),
        ];
        for (reply, expected) in cases {
            let coordinator = coordinator("");
            let transport = CannedTransport::new(reply);

            let results = run(&coordinator, &transport, &missing(), &granting());

            let lookup = results.iter().next().expect("should report");
            assert_eq!(lookup.status, expected, "should map the outcome");
            assert!(lookup.entries.is_empty(), "should carry no entries");
        }
    }

    #[test]
    fn first_party_cookie_id_replaces_the_lookup() {
        let coordinator = coordinator_with(RegisteredProvider::new(
            provider_config(""),
            cookie_provider(SourceInput::Anonymous, Some("vendor_id")),
        ));
        let transport = CannedTransport::new(ok_reply);

        let results = run(&coordinator, &transport, &missing(), &granting());

        let lookup = results.iter().next().expect("should report the provider");
        assert!(
            matches!(lookup.status, LookupStatus::Found { kept: 1, .. }),
            "should keep the cookie UID"
        );
        assert_eq!(
            lookup.entries[0].1[0].uids()[0].id().as_str(),
            "c-1",
            "should store the cookie value"
        );
        assert!(transport.sent.borrow().is_empty(), "should send no lookup");
    }

    fn cookie_coordinator() -> ProviderCoordinator<EchoProvider> {
        coordinator_with(RegisteredProvider::new(
            provider_config(""),
            cookie_provider(SourceInput::Anonymous, Some("vendor_id")),
        ))
    }

    fn held_snapshot(id: &str) -> EcKvSnapshot {
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry
            .ids
            .insert("ids.example.com".to_owned(), provider_record(id));
        seeded_snapshot(&entry)
    }

    #[test]
    fn cookie_opt_out_skips_the_lookup_and_clears_the_sources() {
        let coordinator = cookie_coordinator();
        let transport = CannedTransport::new(ok_reply);

        for snapshot in [missing(), held_snapshot("p-1")] {
            let results = run_with_cookies(
                &coordinator,
                &transport,
                &snapshot,
                &granting(),
                &Limiter(false),
                "vendor_id=optout",
            );

            let lookup = results.iter().next().expect("should report the provider");
            assert_eq!(
                lookup.status,
                LookupStatus::Skipped(SkipReason::OptedOut),
                "should skip for the opt-out"
            );
            assert_eq!(
                lookup.cleared,
                [SourceDomain::try_from("ids.example.com").expect("should parse")],
                "should clear the provider's sources"
            );
        }
        assert!(transport.sent.borrow().is_empty(), "should send no lookup");
    }

    #[test]
    fn changed_cookie_replaces_a_held_live_uid() {
        let coordinator = cookie_coordinator();
        let transport = CannedTransport::new(ok_reply);

        let results = run(&coordinator, &transport, &held_snapshot("p-1"), &granting());

        let lookup = results.iter().next().expect("should report the provider");
        assert!(
            matches!(lookup.status, LookupStatus::Found { kept: 1, .. }),
            "should read the changed cookie although the source is not due"
        );
        assert_eq!(
            lookup.entries[0].1[0].uids()[0].id().as_str(),
            "c-1",
            "should store the new cookie value"
        );
        assert!(transport.sent.borrow().is_empty(), "should send no lookup");
    }

    #[test]
    fn unchanged_cookie_for_a_held_uid_is_not_rewritten() {
        let coordinator = cookie_coordinator();
        let transport = CannedTransport::new(ok_reply);

        let results = run(&coordinator, &transport, &held_snapshot("c-1"), &granting());

        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::NotDue),
            "should skip a cookie repeating the held UID"
        );
        assert!(transport.sent.borrow().is_empty(), "should send no lookup");
    }

    #[test]
    fn missing_or_invalid_cookie_falls_back_to_the_lookup() {
        let coordinator = coordinator_with(RegisteredProvider::new(
            provider_config(""),
            cookie_provider(SourceInput::Anonymous, Some("absent")),
        ));
        let transport = CannedTransport::new(ok_reply);

        let results = run(&coordinator, &transport, &missing(), &granting());

        assert!(
            matches!(only_status(&results), LookupStatus::Found { kept: 1, .. }),
            "should use the lookup"
        );
        assert_eq!(transport.sent.borrow().len(), 1, "should send the lookup");
    }

    #[test]
    fn cookie_is_not_read_without_consent() {
        let coordinator = coordinator_with(RegisteredProvider::new(
            provider_config("tcf_vendor_id = 9999"),
            cookie_provider(SourceInput::Anonymous, Some("vendor_id")),
        ));
        let transport = CannedTransport::new(ok_reply);

        // TCF applies but omits vendor 9999; without a TCF signal the vendor
        // gate does not apply (see
        // `tcf_vendor_id_does_not_gate_traffic_without_tcf`).
        let results = run(
            &coordinator,
            &transport,
            &missing(),
            &tcf_consent(Vec::new()),
        );

        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::Consent),
            "should skip for consent"
        );
    }

    #[test]
    fn disabled_provider_is_not_registered() {
        let coordinator = coordinator("enabled = false");

        assert!(coordinator.is_empty(), "should drop disabled providers");
    }

    #[test]
    fn endpoint_outside_allowlist_is_rejected_at_build() {
        let mut config = provider_config("");
        config.endpoint = Some("https://other.example.com/".to_owned());

        let error = ProviderCoordinator::new(
            vec![RegisteredProvider::new(
                config,
                echo_provider(SourceInput::Anonymous),
            )],
            partners(),
            Duration::from_millis(200),
        )
        .err()
        .expect("should reject endpoint");

        assert!(
            matches!(error.current_context(), ProviderConfigError::Invalid),
            "should report invalid config"
        );
    }

    #[test]
    fn source_outside_capabilities_is_rejected_at_build() {
        let provider = EchoProvider(
            ProviderCapabilities {
                sources: Box::new([]),
                ..echo_provider(SourceInput::Anonymous).0
            },
            None,
        );

        let error = ProviderCoordinator::new(
            vec![RegisteredProvider::new(provider_config(""), provider)],
            partners(),
            Duration::from_millis(200),
        )
        .err()
        .expect("should reject source outside capabilities");

        assert!(
            matches!(error.current_context(), ProviderConfigError::Invalid),
            "should report invalid config"
        );
    }

    fn refreshable_record(from: u64) -> KvPartnerId {
        refreshable_record_owned_by("example-provider", from)
    }

    fn refreshable_record_owned_by(owner: &str, from: u64) -> KvPartnerId {
        let uid = KvUid::new(
            UidValue::try_from("p-1").expect("should build uid"),
            AgentType::try_from(1).expect("should build atype"),
            None,
            Some(ProviderId::new(owner)),
            Expiry::At(UnixSeconds::new(5000)),
        )
        .with_refresh(KvRefresh {
            state: "refresh-state".to_owned(),
            from: UnixSeconds::new(from),
        });
        KvPartnerId::from_eids(
            vec![KvEid::new(EidFields::default(), vec![uid])],
            UnixSeconds::new(1000),
        )
        .expect("should build provider record")
    }

    fn snapshot_holding(record: KvPartnerId) -> EcKvSnapshot {
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry.ids.insert("ids.example.com".to_owned(), record);
        seeded_snapshot(&entry)
    }

    #[test]
    fn a_due_refresh_makes_a_live_source_due() {
        let coordinator = coordinator("");
        let transport = CannedTransport::new(ok_reply);

        let results = run(
            &coordinator,
            &transport,
            &snapshot_holding(refreshable_record(900)),
            &granting(),
        );

        assert!(
            matches!(only_status(&results), LookupStatus::Found { .. }),
            "should refresh a live UID whose refresh time has passed"
        );
        assert_eq!(transport.sent.borrow().len(), 1, "should send the refresh");
    }

    #[test]
    fn a_future_refresh_leaves_the_source_held() {
        let coordinator = coordinator("");
        let transport = CannedTransport::new(ok_reply);

        let results = run(
            &coordinator,
            &transport,
            &snapshot_holding(refreshable_record(2000)),
            &granting(),
        );

        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::NotDue),
            "should not refresh before the refresh time"
        );
        assert!(transport.sent.borrow().is_empty(), "should send nothing");
    }

    #[test]
    fn remembered_opt_out_skips_the_provider_until_it_lapses() {
        // Arrange
        let coordinator = coordinator("");
        let provider = ProviderId::new("example-provider");
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry.opt_outs.insert(provider.clone(), 1001);
        let transport = CannedTransport::new(ok_reply);

        // Act
        let results = run(
            &coordinator,
            &transport,
            &seeded_snapshot(&entry),
            &granting(),
        );

        // Assert
        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::OptedOut),
            "should skip a provider whose opt-out is remembered"
        );
        assert!(
            transport.sent.borrow().is_empty(),
            "should not call a provider the visitor opted out of"
        );

        // Arrange: the opt-out lapsed.
        entry.opt_outs.insert(provider, 1000);

        // Act
        let results = run(
            &coordinator,
            &transport,
            &seeded_snapshot(&entry),
            &granting(),
        );

        // Assert
        assert!(
            matches!(only_status(&results), LookupStatus::Found { .. }),
            "should call the provider again once the opt-out lapses"
        );
    }

    #[test]
    fn another_providers_refresh_state_is_never_handed_over() {
        // Arrange: a live UID with due refresh state written by a provider that
        // no longer owns the source.
        let coordinator = coordinator("");
        let transport = CannedTransport::new(ok_reply);

        // Act
        let results = run(
            &coordinator,
            &transport,
            &snapshot_holding(refreshable_record_owned_by("former-provider", 900)),
            &granting(),
        );

        // Assert
        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::NotDue),
            "should not refresh a UID another provider wrote"
        );
        assert!(
            transport.sent.borrow().is_empty(),
            "should not send another provider's refresh state"
        );
    }

    fn optout_reply() -> Result<TransportResponse, Report<TransportError>> {
        Ok(reply(http::StatusCode::OK, b"optout"))
    }

    #[test]
    fn an_opted_out_reply_clears_the_sources() {
        let coordinator = coordinator("");
        let transport = CannedTransport::new(optout_reply);

        let results = run(&coordinator, &transport, &missing(), &granting());

        let lookup = results.iter().next().expect("should report the provider");
        assert_eq!(
            lookup.status,
            LookupStatus::Skipped(SkipReason::OptedOut),
            "should report the opt-out"
        );
        assert_eq!(
            lookup.cleared,
            [SourceDomain::try_from("ids.example.com").expect("should parse")],
            "should clear the provider's sources"
        );
    }

    /// An echo provider that generates its ID instead of looking it up.
    struct GeneratingProvider(EchoProvider);

    impl IdentityProvider for GeneratingProvider {
        fn capabilities(&self) -> &ProviderCapabilities {
            self.0.capabilities()
        }

        fn build_request(
            &self,
            lookup: &LookupInput<'_>,
        ) -> Result<ProviderRequest, Report<ProviderError>> {
            self.0.build_request(lookup)
        }

        fn parse_response<'body>(
            &self,
            response: ProviderResponse<'body>,
        ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
            self.0.parse_response(response)
        }

        fn generate(&self, _lookup: &LookupInput<'_>) -> Option<Vec<ProviderEid<'static>>> {
            Some(vec![ProviderEid {
                source: Cow::Borrowed("ids.example.com"),
                inserter: None,
                matcher: None,
                mm: None,
                ext: None,
                uids: vec![ProviderUid {
                    id: Cow::Borrowed("generated-1"),
                    atype: None,
                    ext: None,
                    expiry: ProviderExpiry::Unspecified,
                    refresh: None,
                }],
            }])
        }
    }

    fn generating_coordinator() -> ProviderCoordinator<GeneratingProvider> {
        ProviderCoordinator::new(
            vec![RegisteredProvider::new(
                provider_config(""),
                GeneratingProvider(echo_provider(SourceInput::Anonymous)),
            )],
            partners(),
            Duration::from_millis(200),
        )
        .expect("should build coordinator")
    }

    #[test]
    fn a_generated_id_replaces_the_lookup_for_a_due_source() {
        let coordinator = generating_coordinator();
        let transport = CannedTransport::new(ok_reply);

        let results = run(&coordinator, &transport, &missing(), &granting());

        let lookup = results.iter().next().expect("should report the provider");
        assert!(
            matches!(lookup.status, LookupStatus::Found { kept: 1, .. }),
            "should keep the generated UID"
        );
        assert_eq!(
            lookup.entries[0].1[0].uids()[0].id().as_str(),
            "generated-1",
            "should store the generated ID"
        );
        assert!(transport.sent.borrow().is_empty(), "should send no lookup");
    }

    #[test]
    fn a_held_source_is_not_regenerated() {
        let coordinator = generating_coordinator();
        let transport = CannedTransport::new(ok_reply);

        let results = run(
            &coordinator,
            &transport,
            &snapshot_holding(provider_record("p-1")),
            &granting(),
        );

        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::NotDue),
            "should keep the held ID"
        );
    }

    /// An echo provider that makes no lookup.
    struct NoLookupProvider(EchoProvider);

    impl IdentityProvider for NoLookupProvider {
        fn capabilities(&self) -> &ProviderCapabilities {
            self.0.capabilities()
        }

        fn build_request(
            &self,
            _lookup: &LookupInput<'_>,
        ) -> Result<ProviderRequest, Report<ProviderError>> {
            Err(Report::new(ProviderError::Request))
        }

        fn parse_response<'body>(
            &self,
            response: ProviderResponse<'body>,
        ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
            self.0.parse_response(response)
        }
    }

    /// A limiter that counts the checks it is asked for.
    #[derive(Default)]
    struct CountingLimiter(Cell<u32>);

    impl RateLimiter for CountingLimiter {
        fn exceeded(
            &self,
            _key: &str,
            _hourly_limit: u32,
        ) -> Result<bool, Report<TrustedServerError>> {
            self.0.set(self.0.get() + 1);
            Ok(false)
        }
    }

    #[test]
    fn a_provider_making_no_lookup_spends_no_rate_limit() {
        let coordinator = ProviderCoordinator::new(
            vec![
                RegisteredProvider::new(
                    provider_config(""),
                    NoLookupProvider(echo_provider(SourceInput::Anonymous)),
                )
                .with_rate_limit(NonZeroU32::new(1).expect("should be non-zero")),
            ],
            partners(),
            Duration::from_millis(200),
        )
        .expect("should build coordinator");
        let transport = CannedTransport::new(ok_reply);
        let limiter = CountingLimiter::default();

        let results = run_with(&coordinator, &transport, &missing(), &granting(), &limiter);

        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::Input),
            "should skip a provider that builds no request"
        );
        assert_eq!(limiter.0.get(), 0, "should not check the rate limit");
    }

    const SECOND_SOURCE: &str = "ids-b.example.com";

    /// A two-source provider: its cookie fills the first source and its
    /// lookup the second.
    struct TwoSourceProvider(EchoProvider);

    impl IdentityProvider for TwoSourceProvider {
        fn capabilities(&self) -> &ProviderCapabilities {
            self.0.capabilities()
        }

        fn build_request(
            &self,
            lookup: &LookupInput<'_>,
        ) -> Result<ProviderRequest, Report<ProviderError>> {
            self.0.build_request(lookup)
        }

        fn parse_response<'body>(
            &self,
            response: ProviderResponse<'body>,
        ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
            let mut outcome = self.0.parse_response(response)?;
            if let LookupOutcome::Found(eids) = &mut outcome {
                for eid in eids {
                    eid.source = Cow::Borrowed(SECOND_SOURCE);
                }
            }
            Ok(outcome)
        }

        fn read_request_cookies<'cookie>(
            &self,
            cookies: RequestCookies<'cookie>,
        ) -> CookieRead<'cookie> {
            self.0.read_request_cookies(cookies)
        }
    }

    #[test]
    fn a_cookie_spares_only_the_sources_it_fills() {
        let mut provider = cookie_provider(SourceInput::Anonymous, Some("vendor_id"));
        provider.0.sources = Box::new([
            SourceCapability {
                source: SourceDomain::try_from("ids.example.com").expect("should parse"),
                input: SourceInput::Anonymous,
            },
            SourceCapability {
                source: SourceDomain::try_from(SECOND_SOURCE).expect("should parse"),
                input: SourceInput::Anonymous,
            },
        ]);
        let config: IdentityProviderConfig = toml::from_str(&format!(
            "id = \"example-provider\"\nkind = \"echo\"\nendpoint = \"https://ids.example.com/lookup\"\nallowed_domains = [\"ids.example.com\"]\nsources = [\"ids.example.com\", \"{SECOND_SOURCE}\"]\n"
        ))
        .expect("should parse config");
        let partners: Vec<EcPartner> = ["ids.example.com", SECOND_SOURCE]
            .into_iter()
            .map(|source| {
                serde_json::from_value(serde_json::json!({
                    "name": source, "source_domain": source, "bidstream_enabled": true
                }))
                .expect("should build partner")
            })
            .collect();
        let coordinator = ProviderCoordinator::new(
            vec![RegisteredProvider::new(config, TwoSourceProvider(provider))],
            Arc::new(PartnerRegistry::from_config(&partners).expect("should build registry")),
            Duration::from_millis(200),
        )
        .expect("should build coordinator");
        let transport = CannedTransport::new(ok_reply);

        let results = run(&coordinator, &transport, &missing(), &granting());

        assert_eq!(
            *transport.sent.borrow(),
            ["https://ids.example.com/lookup?due=1"],
            "should look up only the source the cookie did not fill"
        );
        let lookup = results.iter().next().expect("should report the provider");
        assert!(
            matches!(lookup.status, LookupStatus::Found { kept: 2, .. }),
            "should keep the cookie and lookup UIDs"
        );
        let mut sources: Vec<&str> = lookup
            .entries
            .iter()
            .map(|(source, _)| source.as_str())
            .collect();
        sources.sort_unstable();
        assert_eq!(
            sources,
            ["ids-b.example.com", "ids.example.com"],
            "should store both sources"
        );
    }
}
