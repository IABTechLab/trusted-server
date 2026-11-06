//! Server-side identity lookup coordination (spec §8).
//!
//! [`ProviderCoordinator`] owns eligibility, consent, destination checks, a
//! deadline-bounded fan-out through a [`LookupTransport`], validation and
//! planning of the navigation's single row change. Providers stay pure.

pub mod results;
pub mod transport;
mod validate;

use std::collections::HashSet;
use std::num::{NonZeroU16, NonZeroU32, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use error_stack::Report;

use self::results::{LookupResults, LookupStatus, OptOutMemory, ProviderLookup, SkipReason};
use self::transport::{
    Deadline, LookupTransport, ProviderIndex, Tagged, TransportError, TransportResponse,
};
use self::validate::{ValidatedEntries, ValidationPolicy, validate};
use crate::consent::ConsentContext;
use crate::consent::jurisdiction::Jurisdiction;
use crate::consent::{allows_eid_persistence, tcf_vendor_consent_denied};
use crate::ec::EcKvSnapshot;
use crate::ec::consent::ec_consent_granted;
use crate::ec::eid_types::{EidFields, Expiry, ProviderId, SourceDomain, UnixSeconds};
use crate::ec::generation::ec_hash;
use crate::ec::kv_types::{KvEid, KvEntry, KvUid};
use crate::ec::provider::{
    ClientSignals, CookieRead, IdentityProvider, LookupInput, LookupOutcome, OwnedEntries,
    ProviderConfigError, ProviderEid, ProviderResponse, RequestCookies, SourceInput,
    destination_allowed, parse_sources, validate_enabled_configs,
};
use crate::ec::rate_limiter::RateLimiter;
use crate::ec::registry::{PULL_SYNC_KIND, PartnerRegistry};

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
    /// Providers whose IDs were generated at dispatch, validated.
    generated: Vec<Tagged<ProviderLookup>>,
    /// Sources each provider may not collect or keep where GDPR applies.
    excluded: Vec<Tagged<Vec<SourceDomain>>>,
    /// Provider state to hand back with each in-flight request's response.
    contexts: Vec<Tagged<String>>,
    /// Providers whose IDs were read from first-party cookies, validated.
    from_cookies: Vec<Tagged<ProviderLookup>>,
}

impl<Pending> Default for PendingLookups<Pending> {
    fn default() -> Self {
        Self {
            requests: Vec::new(),
            deadline: None,
            ec_id: String::new(),
            now: UnixSeconds::new(0),
            early: Vec::new(),
            generated: Vec::new(),
            excluded: Vec::new(),
            contexts: Vec::new(),
            from_cookies: Vec::new(),
        }
    }
}

impl<Pending> std::fmt::Debug for PendingLookups<Pending> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingLookups")
            .field("requests", &self.requests.len())
            .field("early", &self.early.len())
            .field("generated", &self.generated.len())
            .field("from_cookies", &self.from_cookies.len())
            .finish_non_exhaustive()
    }
}

impl<Pending> PendingLookups<Pending> {
    /// Whether nothing was dispatched, generated or skipped.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
            && self.early.is_empty()
            && self.generated.is_empty()
            && self.from_cookies.is_empty()
    }
}

/// A configured provider handed to [`ProviderCoordinator::new`].
pub struct RegisteredProvider<P> {
    /// The provider's configuration entry.
    pub config: IdentityProviderConfig,
    /// The built provider.
    pub provider: P,
}

impl<P> RegisteredProvider<P> {
    /// Pairs a configuration entry with its built provider.
    #[must_use]
    pub fn new(config: IdentityProviderConfig, provider: P) -> Self {
        Self { config, provider }
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
    /// Whether this is a pull-sync partner, counted against
    /// [`ProviderCoordinator::with_pull_sync_concurrency`].
    pull_sync: bool,
}

/// The production coordinator, built once at startup.
///
/// Generic over the provider type: production uses the boxed default, tests
/// and single-provider builds use a concrete type.
pub struct ProviderCoordinator<P = Box<dyn IdentityProvider>> {
    providers: Box<[Registered<P>]>,
    /// Partner config: `openrtb_atype` for validation.
    partners: Arc<PartnerRegistry>,
    budget: Duration,
    /// Most pull-sync lookups dispatched per navigation; `None` is unlimited.
    pull_sync_concurrency: Option<NonZeroUsize>,
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
            pull_sync_concurrency: None,
        })
    }

    /// The order providers are dispatched in: configured providers first,
    /// then pull-sync partners rotated by the hour, so that under
    /// `pull_sync_concurrency` every partner gets a turn, as pull sync did
    /// before it became a provider.
    fn dispatch_order(&self, now: UnixSeconds) -> Vec<usize> {
        let (mut order, mut pull_sync): (Vec<usize>, Vec<usize>) =
            (0..self.providers.len()).partition(|&index| !self.providers[index].policy.pull_sync);
        if !pull_sync.is_empty() {
            let hour = usize::try_from(now.get() / 3600).unwrap_or(0);
            let offset = hour % pull_sync.len();
            pull_sync.rotate_left(offset);
        }
        order.append(&mut pull_sync);
        order
    }

    /// Caps the pull-sync lookups dispatched per navigation
    /// (`ec.pull_sync_concurrency`). Due pull-sync partners past the cap are
    /// skipped as rate limited and retried on a later navigation.
    #[must_use]
    pub fn with_pull_sync_concurrency(mut self, max: Option<NonZeroUsize>) -> Self {
        self.pull_sync_concurrency = max;
        self
    }

    fn register(
        registered: RegisteredProvider<P>,
    ) -> Result<Registered<P>, Report<ProviderConfigError>> {
        let RegisteredProvider { config, provider } = registered;
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
                rate_limit_per_hour: config.rate_limit_per_hour,
                pull_sync: config.kind() == PULL_SYNC_KIND,
            },
            provider,
        })
    }

    /// Whether no provider is enabled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Whether `source` holds a UID of this provider whose refresh is due.
    fn refresh_due(
        registered: &Registered<P>,
        entry: Option<&KvEntry>,
        source: &SourceDomain,
        now: UnixSeconds,
    ) -> bool {
        entry
            .and_then(|entry| entry.ids.get(source.as_str()))
            .is_some_and(|record| {
                record.is_provider_written() && record.refresh_due_for(&registered.id, now)
            })
    }

    /// Whether the row holds a live provider-written UID for `source`.
    fn holds_live_uid(entry: Option<&KvEntry>, source: &str, now: UnixSeconds) -> bool {
        entry
            .and_then(|entry| entry.ids.get(source))
            .is_some_and(|record| record.is_provider_written() && record.has_live_uid(now))
    }

    /// Claimed sources lacking a live provider-written UID (a browser
    /// fallback does not count; for pull sync, any live UID counts), minus
    /// sources needing input V1
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
            let record = entry.and_then(|entry| entry.ids.get(source.as_str()));
            // A UID whose provider asked for a refresh makes its source due;
            // the stored refresh state is the lookup's input.
            if record
                .filter(|record| record.is_provider_written())
                .is_some_and(|record| record.refresh_due_for(&registered.id, now))
            {
                due.push(source.clone());
                continue;
            }
            // A pull-sync source counts any live UID, owned or not, as held:
            // like legacy pull sync, it pulls only for sources with no UID, so
            // an ownerless browser, batch or legacy record does not trigger a
            // lookup (and an EC ID send) on every navigation. Sources with no
            // UID stay bounded by the partner's hourly pull rate limit.
            let held = record.is_some_and(|record| {
                (registered.policy.pull_sync || record.is_provider_written())
                    && record.has_live_uid(now)
            });
            if held {
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

    /// The provider's sources it may not collect or keep for this request:
    /// those it excludes where GDPR applies, when the visitor's jurisdiction
    /// is GDPR. A TCF string alone does not decide this, because some CMPs
    /// set one for every visitor.
    fn excluded_sources(registered: &Registered<P>, consent: &ConsentContext) -> Vec<SourceDomain> {
        if !matches!(consent.jurisdiction, Jurisdiction::Gdpr) {
            return Vec::new();
        }
        registered
            .policy
            .sources
            .iter()
            .filter(|source| registered.provider.excluded_where_gdpr_applies(source))
            .cloned()
            .collect()
    }

    /// Whether the provider's hourly limit is reached. Limiter errors fail
    /// closed, as pull sync always has: an unknown count skips the lookup.
    ///
    /// The bucket is the EC hash (derived from the client IP), not the full
    /// EC ID, so ECs behind one IP share it and a client that clears cookies
    /// cannot reset it.
    fn rate_limited(registered: &Registered<P>, lookup: &LookupContext<'_>) -> bool {
        let Some(limit) = registered.policy.rate_limit_per_hour else {
            return false;
        };
        let key = format!("identity:{}:{}", registered.id, ec_hash(lookup.ec_id));
        match lookup.rate_limiter.exceeded(&key, limit.get()) {
            Ok(exceeded) => exceeded,
            Err(report) => {
                log::warn!(
                    "Identity lookup: provider={} rate limiter failed, skipping lookup: {report:?}",
                    registered.id
                );
                true
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
        let cleared = if matches!(
            status,
            LookupStatus::Skipped(SkipReason::OptedOut | SkipReason::Region)
        ) {
            registered.policy.sources.to_vec()
        } else {
            Vec::new()
        };
        ProviderLookup {
            provider: registered.id.clone(),
            status,
            entries,
            cleared,
            opt_out: OptOutMemory::Unchanged,
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
        // Only 404 is a match-free reply for every provider; a 204 reaches
        // the provider, because some vendors use it to report an opt-out.
        if response.status == http::StatusCode::NOT_FOUND {
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
                // Only an opt-out the vendor's server reports is remembered:
                // it spares the vendor further calls. Cookie opt-outs are
                // re-read on every navigation instead.
                return ProviderLookup {
                    opt_out: OptOutMemory::Remember,
                    ..self.lookup(
                        provider,
                        LookupStatus::Skipped(SkipReason::OptedOut),
                        Vec::new(),
                    )
                };
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

    /// Validates a provider's entries and reports them as one lookup.
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
        // A failed read cannot rule out a withdrawal tombstone, so no provider
        // is contacted.
        let snapshot_failed = matches!(lookup.snapshot, EcKvSnapshot::Failed { .. });
        let entry = lookup.snapshot.entry_for(lookup.ec_id);
        // Missing EC or EID consent, a withdrawal tombstone, an unreadable
        // row (which may hide a tombstone) or no row at all skips every
        // provider: a lookup discloses the EC ID before finalize could
        // decline it, and an EC without a row may never be issued.
        let has_row = !matches!(lookup.snapshot, EcKvSnapshot::Failed { .. }) && entry.is_some();
        let consented = ec_consent_granted(lookup.consent)
            && allows_eid_persistence(lookup.consent)
            && entry.is_some_and(|entry| entry.consent.ok);
        let deadline = Deadline::after(self.budget);
        pending.deadline = Some(deadline);
        let mut pull_sync_dispatched = 0_usize;
        for index in self.dispatch_order(lookup.now) {
            let registered = &self.providers[index];
            let provider = ProviderIndex::new(index);
            let skip = |reason| Tagged {
                provider,
                value: LookupStatus::Skipped(reason),
            };
            if snapshot_failed {
                pending.early.push(skip(SkipReason::SnapshotUnavailable));
                continue;
            }
            let vendor_denied = registered
                .policy
                .tcf_vendor_id
                .is_some_and(|vendor| tcf_vendor_consent_denied(lookup.consent, vendor));
            if !has_row {
                pending.early.push(skip(SkipReason::NoRow));
                continue;
            }
            if !consented || vendor_denied {
                pending.early.push(skip(SkipReason::Consent));
                continue;
            }
            let excluded = Self::excluded_sources(registered, lookup.consent);
            if !excluded.is_empty() {
                if excluded.len() == registered.policy.sources.len() {
                    pending.early.push(skip(SkipReason::Region));
                    continue;
                }
                pending.excluded.push(Tagged {
                    provider,
                    value: excluded.clone(),
                });
            }
            let remembered_opt_out =
                entry.filter(|entry| entry.opted_out(&registered.id, lookup.now));
            let mut opted_back_in = false;
            let (mut due, input_blocked) = Self::due_sources(registered, entry, lookup.now);
            due.retain(|source| !excluded.contains(source));
            // The vendor's own first-party cookies are read on every
            // navigation, so an opt-out or a changed ID is noticed while the
            // sources still hold live UIDs. Their IDs stand in for a lookup.
            match registered.provider.read_request_cookies(
                lookup
                    .cookies
                    .at(lookup.now)
                    .where_gdpr_applies(lookup.consent.gdpr_applies),
            ) {
                CookieRead::OptedOut => {
                    log::debug!(
                        "Identity lookup: provider={} opted out in its cookies",
                        registered.id
                    );
                    pending.early.push(skip(SkipReason::OptedOut));
                    continue;
                }
                CookieRead::Found(eids) => {
                    let mut found = self.validated(provider, eids, lookup.now);
                    // A cookie older than a server-side refresh does not
                    // roll the refreshed UID back.
                    if registered.provider.keeps_fresher_stored_uids() {
                        found.entries.retain(|(source, eids)| {
                            !stored_outlives_cookie(entry, source, eids, lookup.now)
                        });
                        recount_found(&mut found);
                    }
                    if matches!(found.status, LookupStatus::Found { .. }) {
                        // An ID in the vendor's cookies means the visitor
                        // opted back in, so a remembered opt-out is
                        // forgotten.
                        if remembered_opt_out.is_some() {
                            opted_back_in = true;
                            found.opt_out = OptOutMemory::Forget;
                        }
                        // Only the sources the cookies filled are spared a
                        // lookup; the provider's other due sources are
                        // still looked up.
                        // A refresh-due source whose cookie only repeats the
                        // held UID stays due, so its refresh still runs before
                        // the UID expires.
                        due.retain(|source| {
                            found
                                .entries
                                .iter()
                                .find(|(filled, _)| filled == source)
                                .is_none_or(|(_, eids)| {
                                    Self::refresh_due(registered, entry, source, lookup.now)
                                        && holds_same_uids_for(entry, source, eids, lookup.now)
                                })
                        });
                        // A source whose cookie repeats the held UIDs is not
                        // rewritten: its relative expiry would cause a write
                        // on every navigation, and a failed lookup for a
                        // still-due source would replace the held record and
                        // its refresh state.
                        found.entries.retain(|(source, eids)| {
                            !holds_same_uids_for(entry, source, eids, lookup.now)
                        });
                        recount_found(&mut found);
                        if !found.entries.is_empty() || opted_back_in {
                            pending.from_cookies.push(Tagged {
                                provider,
                                value: found,
                            });
                            if due.is_empty() {
                                continue;
                            }
                        }
                    }
                }
                CookieRead::Absent => {}
            }
            // A remembered opt-out keeps the provider from being called until
            // it lapses, unless its cookies hold an ID again. It clears only
            // sources the row still stores, so once they are gone the
            // after-response path skips its re-read.
            if let Some(entry) = remembered_opt_out.filter(|_| !opted_back_in) {
                pending.generated.push(Tagged {
                    provider,
                    value: ProviderLookup {
                        provider: registered.id.clone(),
                        status: LookupStatus::Skipped(SkipReason::OptedOut),
                        entries: Vec::new(),
                        cleared: registered
                            .policy
                            .sources
                            .iter()
                            .filter(|source| entry.ids.contains_key(source.as_str()))
                            .cloned()
                            .collect(),
                        opt_out: OptOutMemory::Unchanged,
                    },
                });
                continue;
            }
            if due.is_empty() {
                pending.early.push(skip(if input_blocked {
                    SkipReason::Input
                } else {
                    SkipReason::NotDue
                }));
                continue;
            }
            if registered.policy.pull_sync
                && self
                    .pull_sync_concurrency
                    .is_some_and(|max| pull_sync_dispatched >= max.get())
            {
                pending.early.push(skip(SkipReason::RateLimited));
                continue;
            }
            let input = LookupInput {
                ec_id: lookup.ec_id,
                consent: lookup.consent,
                client: lookup.client,
                publisher_domain: lookup.publisher_domain,
                due_sources: &due,
                current: OwnedEntries::new(entry, &registered.policy.sources, &registered.id),
                cookies: lookup
                    .cookies
                    .at(lookup.now)
                    .where_gdpr_applies(lookup.consent.gdpr_applies),
                now: lookup.now,
                hashed_email: None,
            };
            if let Some(mut eids) = registered.provider.generate(&input) {
                // Only a due source without a live UID may take a generated
                // ID: a refresh-due source keeps its UID and goes to lookup.
                eids.retain(|eid| {
                    SourceDomain::try_from(eid.source.as_ref()).is_ok_and(|source| {
                        due.contains(&source)
                            && !Self::holds_live_uid(entry, source.as_str(), lookup.now)
                    })
                });
                if !eids.is_empty() {
                    pending.generated.push(Tagged {
                        provider,
                        value: self.validated(provider, eids, lookup.now),
                    });
                    continue;
                }
            }
            // The request is built before the rate limit is checked, so a
            // provider that makes no lookup spends none of its budget.
            let mut request = match registered.provider.build_request(&input) {
                Ok(request) => request,
                Err(report) => {
                    log::warn!(
                        "Identity lookup: provider={} request not built: {}",
                        registered.id,
                        report.current_context()
                    );
                    pending.early.push(Tagged {
                        provider,
                        value: LookupStatus::RequestFailed,
                    });
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
                    pull_sync_dispatched += usize::from(registered.policy.pull_sync);
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
            generated,
            excluded,
            contexts,
            from_cookies,
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
        for Tagged { provider, value } in from_cookies.into_iter().chain(generated) {
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
        for Tagged { provider, value } in excluded {
            if let Some(lookup) = slots[provider.get()].as_mut() {
                exclude_sources(lookup, value);
            }
        }
        let lookups: Vec<ProviderLookup> = slots.into_iter().flatten().collect();
        for lookup in &lookups {
            log_outcome(lookup);
        }
        LookupResults::new(&ec_id, lookups)
    }
}

/// Drops `sources` from a provider's result and removes them from the row.
fn exclude_sources(lookup: &mut ProviderLookup, sources: Vec<SourceDomain>) {
    lookup
        .entries
        .retain(|(source, _)| !sources.contains(source));
    recount_found(lookup);
    for source in sources {
        if !lookup.cleared.contains(&source) {
            lookup.cleared.push(source);
        }
    }
}

/// Merges a provider's cookie result with its later lookup or generated
/// result for the sources the cookies did not fill.
///
/// An opt-out wins; otherwise the found entries of both are kept, and a
/// later result that found nothing leaves the cookie result.
fn merge_lookups(earlier: ProviderLookup, later: ProviderLookup) -> ProviderLookup {
    // A server opt-out is remembered over anything; otherwise the cookies'
    // opt-back-in is kept.
    let opt_out = match (earlier.opt_out, later.opt_out) {
        (_, OptOutMemory::Remember) => OptOutMemory::Remember,
        (OptOutMemory::Forget, _) => OptOutMemory::Forget,
        (_, memory) => memory,
    };
    ProviderLookup {
        opt_out,
        ..merge_entries(earlier, later)
    }
}

/// Merges the statuses and entries of [`merge_lookups`].
fn merge_entries(earlier: ProviderLookup, later: ProviderLookup) -> ProviderLookup {
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

/// Recounts the UIDs a found result keeps after some of its entries were
/// dropped; a result left with none becomes `NoMatch`.
fn recount_found(lookup: &mut ProviderLookup) {
    if let LookupStatus::Found { kept, .. } = &mut lookup.status {
        let remaining = lookup
            .entries
            .iter()
            .flat_map(|(_, eids)| eids)
            .map(|eid| eid.uids().len())
            .sum::<usize>();
        *kept = u16::try_from(remaining).unwrap_or(u16::MAX);
        if remaining == 0 {
            lookup.status = LookupStatus::NoMatch;
        }
    }
}

/// Whether the row holds a live UID for `source` whose absolute expiry is
/// later than that of every UID in `eids`, so the cookie is older than the
/// stored record.
fn stored_outlives_cookie(
    entry: Option<&KvEntry>,
    source: &SourceDomain,
    eids: &[KvEid],
    now: UnixSeconds,
) -> bool {
    let Some(record) = entry.and_then(|entry| entry.ids.get(source.as_str())) else {
        return false;
    };
    let Some(stored_until) = record
        .eids()
        .iter()
        .flat_map(KvEid::uids)
        .filter(|uid| uid.is_live(now))
        .filter_map(|uid| match uid.expiry() {
            Expiry::At(at) => Some(at),
            Expiry::Never => None,
        })
        .max()
    else {
        return false;
    };
    eids.iter()
        .flat_map(KvEid::uids)
        .all(|uid| match uid.expiry() {
            Expiry::At(at) => at < stored_until,
            Expiry::Never => false,
        })
}

/// Whether the row holds exactly the live UIDs in `eids` for `source`.
///
/// UIDs are compared by ID, agent type, `ext` and their EID-level fields;
/// only expiry and refresh state are ignored, so a cookie that changes any
/// stored field is still written.
fn holds_same_uids_for(
    entry: Option<&KvEntry>,
    source: &SourceDomain,
    eids: &[KvEid],
    now: UnixSeconds,
) -> bool {
    let Some(record) = entry.and_then(|entry| entry.ids.get(source.as_str())) else {
        return false;
    };
    let held: Vec<(&EidFields, &KvUid)> = record
        .eids()
        .iter()
        .flat_map(|eid| eid.uids().iter().map(move |uid| (eid.fields(), uid)))
        .filter(|(_, uid)| uid.is_live(now))
        .collect();
    let read: Vec<(&EidFields, &KvUid)> = eids
        .iter()
        .flat_map(|eid| eid.uids().iter().map(move |uid| (eid.fields(), uid)))
        .collect();
    let same = |(left_fields, left): &(&EidFields, &KvUid),
                (right_fields, right): &(&EidFields, &KvUid)| {
        left_fields == right_fields
            && left.id() == right.id()
            && left.atype() == right.atype()
            && left.ext() == right.ext()
    };
    held.len() == read.len()
        && read
            .iter()
            .all(|read_uid| held.iter().any(|held_uid| same(held_uid, read_uid)))
        && held
            .iter()
            .all(|held_uid| read.iter().any(|read_uid| same(held_uid, read_uid)))
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
    use crate::consent::types::TcfConsent;
    use crate::ec::coordinator::results::DropCounts;
    use crate::ec::eid_types::{AgentType, UidValue};
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
            // Like LiveRamp, the echo vendor reports an opt-out with a 204.
            if response.status == http::StatusCode::NO_CONTENT {
                return Ok(LookupOutcome::OptedOut);
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

    fn coordinator_with<P: IdentityProvider>(
        registered: RegisteredProvider<P>,
    ) -> ProviderCoordinator<P> {
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

    /// A consented row with no IDs yet.
    fn live_row() -> EcKvSnapshot {
        let mut entry = crate::ec::kv_types::KvEntry::tombstone(1000);
        entry.consent.ok = true;
        EcKvSnapshot::Present {
            ec_id: EC_ID.to_owned(),
            entry: Box::new(entry),
            generation: Some(1),
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

        let results = run(&coordinator, &transport, &live_row(), &granting());

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

    #[test]
    fn ec_without_a_row_is_not_sent_to_vendors() {
        // Arrange: a well-formed `ts-ec` cookie whose row does not exist.
        let coordinator = coordinator("");
        let transport = CannedTransport::new(ok_reply);

        // Act
        let results = run(&coordinator, &transport, &missing(), &granting());

        // Assert
        assert_eq!(
            results
                .iter()
                .next()
                .expect("should report the provider")
                .status,
            LookupStatus::Skipped(SkipReason::NoRow),
            "should skip a provider for an EC with no row"
        );
        assert!(transport.sent.borrow().is_empty(), "should send nothing");
    }

    fn provider_record(id: &str) -> KvPartnerId {
        let uid = KvUid::new(
            UidValue::try_from(id).expect("should build uid"),
            AgentType::try_from(3).expect("should build atype"),
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

    fn pull_sync_pair(max: Option<NonZeroUsize>) -> ProviderCoordinator<EchoProvider> {
        let sources = ["a.example.com", "b.example.com"];
        let partners: Vec<EcPartner> = sources
            .iter()
            .map(|source| {
                serde_json::from_value(serde_json::json!({
                    "name": source, "source_domain": source
                }))
                .expect("should build partner")
            })
            .collect();
        let registered = sources
            .iter()
            .map(|source| {
                let config: IdentityProviderConfig = toml::from_str(&format!(
                    "id = \"pull-sync:{source}\"\nkind = \"{PULL_SYNC_KIND}\"\nendpoint = \"https://ids.example.com/lookup\"\nallowed_domains = [\"ids.example.com\"]\nsources = [\"{source}\"]"
                ))
                .expect("should parse config");
                let mut provider = echo_provider(SourceInput::Anonymous);
                provider.0.sources = Box::new([SourceCapability {
                    source: SourceDomain::try_from(*source).expect("should parse"),
                    input: SourceInput::Anonymous,
                }]);
                RegisteredProvider::new(config, provider)
            })
            .collect();
        ProviderCoordinator::new(
            registered,
            Arc::new(PartnerRegistry::from_config(&partners).expect("should build registry")),
            Duration::from_millis(200),
        )
        .expect("should build coordinator")
        .with_pull_sync_concurrency(max)
    }

    #[test]
    fn pull_sync_concurrency_caps_dispatched_lookups() {
        let coordinator = pull_sync_pair(NonZeroUsize::new(1));
        let transport = CannedTransport::new(not_found_reply);

        let results = run(&coordinator, &transport, &missing(), &granting());

        assert_eq!(transport.sent.borrow().len(), 1, "should send one lookup");
        let statuses: Vec<LookupStatus> = results.iter().map(|lookup| lookup.status).collect();
        assert_eq!(
            statuses[1],
            LookupStatus::Skipped(SkipReason::RateLimited),
            "should skip the pull-sync lookup past the cap"
        );
    }

    #[test]
    fn pull_sync_rotates_the_partner_past_the_cap_by_the_hour() {
        let coordinator = pull_sync_pair(NonZeroUsize::new(1));

        let first_hour = coordinator.dispatch_order(UnixSeconds::new(0));
        let next_hour = coordinator.dispatch_order(UnixSeconds::new(3600));

        assert_ne!(
            first_hour.first(),
            next_hour.first(),
            "should give the other partner the first slot the next hour"
        );
        let mut sorted = next_hour.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 1], "should still dispatch every provider");
    }

    #[test]
    fn pull_sync_holds_ownerless_uids_without_repulling() {
        let coordinator = pull_sync_pair(None);
        let transport = CannedTransport::new(not_found_reply);
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry
            .ids
            .insert("a.example.com".to_owned(), KvPartnerId::single("legacy"));
        let snapshot = seeded_snapshot(&entry);

        for _ in 0..2 {
            run(&coordinator, &transport, &snapshot, &granting());
        }

        assert_eq!(
            transport.sent.borrow().len(),
            2,
            "should look up only the empty source, once per navigation, after a no-match"
        );
        assert!(
            transport
                .sent
                .borrow()
                .iter()
                .all(|url| url.ends_with("due=1")),
            "should never include the ownerless source"
        );
    }

    #[test]
    fn pull_sync_drops_oversized_uid() {
        let config: IdentityProviderConfig = toml::from_str(&format!(
            "id = \"pull-sync:ids.example.com\"\nkind = \"{PULL_SYNC_KIND}\"\nendpoint = \"https://ids.example.com/lookup\"\nallowed_domains = [\"ids.example.com\"]\nsources = [\"ids.example.com\"]"
        ))
        .expect("should parse config");
        let coordinator = coordinator_with(RegisteredProvider::new(
            config,
            echo_provider(SourceInput::Anonymous),
        ));
        let transport = CannedTransport::new(|| {
            Ok(TransportResponse {
                status: http::StatusCode::OK,
                headers: http::HeaderMap::new(),
                body: bytes::Bytes::from("x".repeat(crate::ec::kv_types::MAX_UID_LENGTH + 1)),
            })
        });

        let results = run(&coordinator, &transport, &missing(), &granting());

        let lookup = results.iter().next().expect("should report the provider");
        assert_eq!(
            lookup.status,
            LookupStatus::NoMatch,
            "should treat a pull with only an oversized UID as no match"
        );
        assert!(lookup.entries.is_empty(), "should store no oversized UID");
    }

    #[test]
    fn pull_sync_concurrency_none_is_unlimited() {
        let coordinator = pull_sync_pair(None);
        let transport = CannedTransport::new(not_found_reply);

        run(&coordinator, &transport, &missing(), &granting());

        assert_eq!(transport.sent.borrow().len(), 2, "should send both lookups");
    }

    #[test]
    fn failed_snapshot_skips_every_provider() {
        let coordinator = coordinator("");
        let transport = CannedTransport::new(ok_reply);

        let results = run(
            &coordinator,
            &transport,
            &EcKvSnapshot::Failed {
                ec_id: EC_ID.to_owned(),
            },
            &granting(),
        );

        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::SnapshotUnavailable),
            "should skip when the snapshot read failed"
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
    fn tcf_vendor_id_does_not_gate_traffic_without_tcf() {
        let coordinator = coordinator("tcf_vendor_id = 9999");
        let transport = CannedTransport::new(ok_reply);

        let results = run(&coordinator, &transport, &live_row(), &granting());

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

        let results = run(&coordinator, &transport, &live_row(), &tcf_consent(vec![1]));

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
            &live_row(),
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

        fn excluded_where_gdpr_applies(&self, _source: &SourceDomain) -> bool {
            true
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

        let gdpr = run(&coordinator, &transport, &live_row(), &tcf_consent(vec![]));
        let outside = run(&coordinator, &transport, &live_row(), &granting());

        let lookup = gdpr.iter().next().expect("should report the provider");
        assert_eq!(
            lookup.status,
            LookupStatus::Skipped(SkipReason::Region),
            "should neither read cookies nor look up where GDPR applies"
        );
        assert_eq!(
            lookup.cleared,
            [SourceDomain::try_from("ids.example.com").expect("should parse")],
            "should remove the stored UIDs where GDPR applies"
        );
        assert!(
            matches!(only_status(&outside), LookupStatus::Found { .. }),
            "should read the cookie where GDPR does not apply"
        );
    }

    #[test]
    fn a_tcf_string_alone_does_not_exclude_a_provider_outside_gdpr() {
        // Arrange: a CMP that sets a TCF string for a US visitor.
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
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            ..tcf_consent(vec![])
        };

        // Act
        let results = run(&coordinator, &transport, &live_row(), &consent);

        // Assert
        assert!(
            matches!(only_status(&results), LookupStatus::Found { .. }),
            "should collect where only a TCF string is present"
        );
    }

    #[test]
    fn hashed_email_sources_are_skipped_for_input() {
        let coordinator = coordinator_with(RegisteredProvider::new(
            provider_config(""),
            echo_provider(SourceInput::HashedEmail),
        ));
        let transport = CannedTransport::new(ok_reply);

        let results = run(&coordinator, &transport, &live_row(), &granting());

        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::Input),
            "should skip sources needing hashed email"
        );
    }

    #[test]
    fn rate_limited_provider_is_skipped() {
        let coordinator = coordinator_with(RegisteredProvider::new(
            provider_config("rate_limit_per_hour = 1"),
            echo_provider(SourceInput::Anonymous),
        ));
        let transport = CannedTransport::new(ok_reply);

        let results = run_with(
            &coordinator,
            &transport,
            &live_row(),
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

    struct FailingLimiter;

    impl RateLimiter for FailingLimiter {
        fn exceeded(
            &self,
            _key: &str,
            _hourly_limit: u32,
        ) -> Result<bool, Report<TrustedServerError>> {
            Err(Report::new(TrustedServerError::KvStore {
                store_name: "example-counter".to_owned(),
                message: "unavailable".to_owned(),
            }))
        }
    }

    #[test]
    fn limiter_failure_skips_the_lookup() {
        let coordinator = coordinator_with(RegisteredProvider::new(
            provider_config("rate_limit_per_hour = 1"),
            echo_provider(SourceInput::Anonymous),
        ));
        let transport = CannedTransport::new(ok_reply);

        let results = run_with(
            &coordinator,
            &transport,
            &missing(),
            &granting(),
            &FailingLimiter,
        );

        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::RateLimited),
            "should skip when the limiter cannot count"
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

        let results = run(&coordinator, &transport, &live_row(), &granting());

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

            let results = run(&coordinator, &transport, &live_row(), &granting());

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

        let results = run(&coordinator, &transport, &live_row(), &granting());

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

        for snapshot in [live_row(), held_snapshot("p-1")] {
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

        let results = run(&coordinator, &transport, &live_row(), &granting());

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

        let results = run(&coordinator, &transport, &live_row(), &tcf_consent(vec![]));

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
            AgentType::try_from(3).expect("should build atype"),
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
    fn a_due_refresh_runs_while_the_cookie_repeats_the_held_uid() {
        let coordinator = cookie_coordinator();
        let transport = CannedTransport::new(ok_reply);

        run_with_cookies(
            &coordinator,
            &transport,
            &snapshot_holding(refreshable_record(900)),
            &granting(),
            &Limiter(false),
            "vendor_id=p-1",
        );

        assert_eq!(
            transport.sent.borrow().len(),
            1,
            "should send the refresh although the cookie holds the same UID"
        );
    }

    #[test]
    fn a_newer_cookie_uid_replaces_a_refresh() {
        let coordinator = cookie_coordinator();
        let transport = CannedTransport::new(ok_reply);

        run_with_cookies(
            &coordinator,
            &transport,
            &snapshot_holding(refreshable_record(900)),
            &granting(),
            &Limiter(false),
            "vendor_id=c-2",
        );

        assert!(
            transport.sent.borrow().is_empty(),
            "should take the newer cookie UID instead of refreshing"
        );
    }

    fn found_lookup(source: &str, id: &str) -> ProviderLookup {
        let uid = KvUid::new(
            UidValue::try_from(id).expect("should build uid"),
            AgentType::try_from(1).expect("should build atype"),
            None,
            Some(ProviderId::new("example-provider")),
            Expiry::Never,
        );
        ProviderLookup {
            provider: ProviderId::new("example-provider"),
            status: LookupStatus::Found {
                kept: 1,
                dropped: DropCounts::default(),
            },
            entries: vec![(
                SourceDomain::try_from(source).expect("should build source"),
                vec![KvEid::new(EidFields::default(), vec![uid])],
            )],
            cleared: Vec::new(),
            opt_out: OptOutMemory::Unchanged,
        }
    }

    fn status_lookup(status: LookupStatus) -> ProviderLookup {
        ProviderLookup {
            provider: ProviderId::new("example-provider"),
            status,
            entries: Vec::new(),
            cleared: Vec::new(),
            opt_out: OptOutMemory::Unchanged,
        }
    }

    #[test]
    fn merging_keeps_cookie_ids_when_the_lookup_fails_and_lets_an_opt_out_win() {
        for failed in [
            LookupStatus::Timeout,
            LookupStatus::HttpError(http::StatusCode::BAD_GATEWAY),
        ] {
            let merged = merge_lookups(found_lookup("a.example.com", "c-1"), status_lookup(failed));
            assert!(
                matches!(merged.status, LookupStatus::Found { kept: 1, .. }),
                "should keep the cookie IDs when the lookup fails"
            );
        }

        let merged = merge_lookups(
            found_lookup("a.example.com", "c-1"),
            status_lookup(LookupStatus::Skipped(SkipReason::OptedOut)),
        );

        assert_eq!(
            merged.status,
            LookupStatus::Skipped(SkipReason::OptedOut),
            "should let a lookup opt-out win over cookie IDs"
        );
    }

    #[test]
    fn merging_combines_cookie_and_lookup_sources() {
        let merged = merge_lookups(
            found_lookup("a.example.com", "c-1"),
            found_lookup("b.example.com", "l-1"),
        );

        assert!(
            matches!(merged.status, LookupStatus::Found { kept: 2, .. }),
            "should count both sources"
        );
        assert_eq!(merged.entries.len(), 2, "should keep both sources");
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
    fn a_cookie_id_clears_a_remembered_opt_out() {
        // Arrange: the opt-out is remembered for 29 more days, and the
        // vendor's cookie holds an ID again.
        let coordinator = cookie_coordinator();
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry.opt_outs.insert(
            ProviderId::new("example-provider"),
            1000 + 29 * 24 * 60 * 60,
        );
        let transport = CannedTransport::new(ok_reply);

        // Act
        let results = run_with_cookies(
            &coordinator,
            &transport,
            &seeded_snapshot(&entry),
            &granting(),
            &Limiter(false),
            "vendor_id=c-1",
        );

        // Assert
        let lookup = results.iter().next().expect("should report the provider");
        assert!(
            matches!(lookup.status, LookupStatus::Found { kept: 1, .. }),
            "should store the cookie ID despite the remembered opt-out"
        );
        assert_eq!(
            lookup.opt_out,
            OptOutMemory::Forget,
            "should forget the remembered opt-out"
        );
        assert!(transport.sent.borrow().is_empty(), "should send nothing");
    }

    #[test]
    fn only_a_vendor_server_opt_out_is_remembered() {
        let cookie_only = cookie_coordinator();
        let cookie = run_with_cookies(
            &cookie_only,
            &CannedTransport::new(ok_reply),
            &live_row(),
            &granting(),
            &Limiter(false),
            "vendor_id=optout",
        );
        let server = run(
            &coordinator(""),
            &CannedTransport::new(optout_reply),
            &live_row(),
            &granting(),
        );

        let memory = |results: &LookupResults| {
            results
                .iter()
                .next()
                .expect("should report the provider")
                .opt_out
        };
        assert_eq!(
            memory(&cookie),
            OptOutMemory::Unchanged,
            "should not remember a cookie opt-out"
        );
        assert_eq!(
            memory(&server),
            OptOutMemory::Remember,
            "should remember a vendor server's opt-out"
        );
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
        assert!(
            results
                .iter()
                .next()
                .expect("should report the provider")
                .cleared
                .is_empty(),
            "should clear nothing when the row stores none of its sources"
        );

        // Arrange: a UID for a claimed source is still stored.
        let mut stored = entry.clone();
        stored
            .ids
            .insert("ids.example.com".to_owned(), KvPartnerId::single("p-1"));

        // Act
        let results = run(
            &coordinator,
            &transport,
            &seeded_snapshot(&stored),
            &granting(),
        );

        // Assert
        assert_eq!(
            results
                .iter()
                .next()
                .expect("should report the provider")
                .cleared,
            [SourceDomain::try_from("ids.example.com").expect("should parse")],
            "should clear the claimed source the row still stores"
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

        let results = run(&coordinator, &transport, &live_row(), &granting());

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

    fn no_content_reply() -> Result<TransportResponse, Report<TransportError>> {
        Ok(reply(http::StatusCode::NO_CONTENT, b""))
    }

    #[test]
    fn a_204_reply_reaches_the_provider() {
        let coordinator = coordinator("");
        let transport = CannedTransport::new(no_content_reply);

        let results = run(&coordinator, &transport, &live_row(), &granting());

        let lookup = results.iter().next().expect("should report the provider");
        assert_eq!(
            lookup.status,
            LookupStatus::Skipped(SkipReason::OptedOut),
            "should report the opt-out the provider reads from a 204"
        );
        assert_eq!(
            lookup.cleared,
            [SourceDomain::try_from("ids.example.com").expect("should parse")],
            "should clear the provider's sources"
        );
    }

    #[test]
    fn a_404_reply_is_no_match_without_parsing() {
        let coordinator = coordinator("");
        let transport = CannedTransport::new(not_found_reply);

        let results = run(&coordinator, &transport, &live_row(), &granting());

        assert_eq!(
            only_status(&results),
            LookupStatus::NoMatch,
            "should report a 404 as no match"
        );
    }

    /// An echo provider whose request can never be built.
    struct UnbuildableProvider(EchoProvider);

    impl IdentityProvider for UnbuildableProvider {
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

    #[test]
    fn a_request_the_provider_cannot_build_is_reported_as_a_failure() {
        let coordinator = ProviderCoordinator::new(
            vec![RegisteredProvider::new(
                provider_config(""),
                UnbuildableProvider(echo_provider(SourceInput::Anonymous)),
            )],
            partners(),
            Duration::from_millis(200),
        )
        .expect("should build coordinator");
        let transport = CannedTransport::new(ok_reply);

        let results = run(&coordinator, &transport, &live_row(), &granting());

        assert_eq!(
            only_status(&results),
            LookupStatus::RequestFailed,
            "should report a provider bug instead of a skip"
        );
    }

    /// An echo provider that generates its ID for the given source instead of
    /// looking it up.
    struct GeneratingProvider(EchoProvider, &'static str);

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
                source: Cow::Borrowed(self.1),
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
        generating_coordinator_for("ids.example.com")
    }

    fn generating_coordinator_for(source: &'static str) -> ProviderCoordinator<GeneratingProvider> {
        ProviderCoordinator::new(
            vec![RegisteredProvider::new(
                provider_config(""),
                GeneratingProvider(echo_provider(SourceInput::Anonymous), source),
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

        let results = run(&coordinator, &transport, &live_row(), &granting());

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
    fn a_generated_id_matches_its_due_source_after_normalizing() {
        let coordinator = generating_coordinator_for("IDS.example.com");
        let transport = CannedTransport::new(ok_reply);

        let results = run(&coordinator, &transport, &live_row(), &granting());

        let lookup = results.iter().next().expect("should report the provider");
        assert!(
            matches!(lookup.status, LookupStatus::Found { kept: 1, .. }),
            "should keep the generated UID for a differently cased source"
        );
        assert!(transport.sent.borrow().is_empty(), "should send no lookup");
    }

    #[test]
    fn a_refresh_due_live_source_is_looked_up_not_regenerated() {
        let coordinator = generating_coordinator();
        let transport = CannedTransport::new(ok_reply);

        let results = run(
            &coordinator,
            &transport,
            &snapshot_holding(refreshable_record(900)),
            &granting(),
        );

        let lookup = results.iter().next().expect("should report the provider");
        assert!(
            lookup
                .entries
                .iter()
                .flat_map(|(_, eids)| eids)
                .flat_map(KvEid::uids)
                .all(|uid| uid.id().as_str() != "generated-1"),
            "should not replace the live UID with a generated one"
        );
        assert_eq!(
            transport.sent.borrow().len(),
            1,
            "should send the refresh lookup instead"
        );
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
            vec![RegisteredProvider::new(
                provider_config("rate_limit_per_hour = 1"),
                NoLookupProvider(echo_provider(SourceInput::Anonymous)),
            )],
            partners(),
            Duration::from_millis(200),
        )
        .expect("should build coordinator");
        let transport = CannedTransport::new(ok_reply);
        let limiter = CountingLimiter::default();

        let results = run_with(&coordinator, &transport, &live_row(), &granting(), &limiter);

        assert_eq!(
            only_status(&results),
            LookupStatus::RequestFailed,
            "should report a provider that builds no request"
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

    fn two_source_coordinator<P: IdentityProvider>(
        wrap: impl FnOnce(TwoSourceProvider) -> P,
    ) -> ProviderCoordinator<P> {
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
            "id = \"example-provider\"\nkind = \"echo\"\nendpoint = \"https://ids.example.com/lookup\"\nallowed_domains = [\"ids.example.com\"]\nsources = [\"ids.example.com\", \"{SECOND_SOURCE}\"]\nrate_limit_per_hour = 1\n"
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
        ProviderCoordinator::new(
            vec![RegisteredProvider::new(
                config,
                wrap(TwoSourceProvider(provider)),
            )],
            Arc::new(PartnerRegistry::from_config(&partners).expect("should build registry")),
            Duration::from_millis(200),
        )
        .expect("should build coordinator")
    }

    #[test]
    fn a_cookie_spares_only_the_sources_it_fills() {
        let coordinator = two_source_coordinator(|provider| provider);
        let transport = CannedTransport::new(ok_reply);

        let results = run(&coordinator, &transport, &live_row(), &granting());

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

    /// Plans the row change a navigation's results make, as finalize does.
    fn planned_change(
        snapshot: &EcKvSnapshot,
        results: &LookupResults,
    ) -> Option<crate::ec::page_load::RowChange> {
        let partners: Vec<EcPartner> = ["ids.example.com", SECOND_SOURCE]
            .into_iter()
            .map(|source| {
                serde_json::from_value(serde_json::json!({
                    "name": source, "source_domain": source, "bidstream_enabled": true
                }))
                .expect("should build partner")
            })
            .collect();
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        crate::ec::write_plan::plan_write(&crate::ec::write_plan::WritePlanInput {
            ec_id: EC_ID,
            snapshot,
            new_entry: None,
            browser_ingest: &[],
            lookups: results,
            registry: &registry,
            now: UnixSeconds::new(1000),
        })
    }

    /// Outcomes of a lookup that stores nothing, with a label for messages.
    fn failed_outcomes() -> [(&'static str, Reply, bool); 5] {
        [
            ("404", not_found_reply, false),
            ("timeout", timeout_reply, false),
            ("502", server_error_reply, false),
            ("invalid", invalid_reply, false),
            ("rate-limited", ok_reply, true),
        ]
    }

    #[test]
    fn an_unchanged_cookie_is_not_rewritten_while_another_source_is_due() {
        // Arrange: the row holds the cookie's UID; the second source is due.
        let coordinator = two_source_coordinator(|provider| provider);
        let snapshot = held_snapshot("c-1");

        for (label, reply, limited) in failed_outcomes() {
            for cookies in ["vendor_id=c-1", ""] {
                let transport = CannedTransport::new(reply);

                // Act
                let results = run_with_cookies(
                    &coordinator,
                    &transport,
                    &snapshot,
                    &granting(),
                    &Limiter(limited),
                    cookies,
                );

                // Assert
                assert!(
                    planned_change(&snapshot, &results).is_none(),
                    "should plan no write for {label} with cookies `{cookies}`: {:?}",
                    only_status(&results)
                );
            }
        }
    }

    #[test]
    fn a_failed_refresh_keeps_the_held_refresh_state() {
        // Arrange: the held UID's refresh is due and the cookie repeats it.
        let coordinator = coordinator_with(RegisteredProvider::new(
            provider_config("rate_limit_per_hour = 1"),
            cookie_provider(SourceInput::Anonymous, Some("vendor_id")),
        ));
        let snapshot = snapshot_holding(refreshable_record(900));

        for (label, reply, limited) in failed_outcomes() {
            for cookies in ["vendor_id=p-1", ""] {
                let transport = CannedTransport::new(reply);

                // Act
                let results = run_with_cookies(
                    &coordinator,
                    &transport,
                    &snapshot,
                    &granting(),
                    &Limiter(limited),
                    cookies,
                );

                // Assert
                let lookup = results.iter().next().expect("should report the provider");
                assert!(
                    lookup.entries.is_empty(),
                    "should not replace the held record after {label}: {:?}",
                    lookup.status
                );
                assert!(
                    planned_change(&snapshot, &results).is_none(),
                    "should plan no write that drops the refresh state for {label} with cookies `{cookies}`"
                );
            }
        }
    }

    /// A cookie provider that keeps a stored UID outliving the cookie's.
    struct RefreshingCookieProvider(EchoProvider);

    impl IdentityProvider for RefreshingCookieProvider {
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

        fn keeps_fresher_stored_uids(&self) -> bool {
            true
        }
    }

    fn record_until(id: &str, until: u64) -> KvPartnerId {
        let uid = KvUid::new(
            UidValue::try_from(id).expect("should build uid"),
            AgentType::try_from(3).expect("should build atype"),
            None,
            Some(ProviderId::new("example-provider")),
            Expiry::At(UnixSeconds::new(until)),
        );
        KvPartnerId::from_eids(
            vec![KvEid::new(EidFields::default(), vec![uid])],
            UnixSeconds::new(1000),
        )
        .expect("should build provider record")
    }

    #[test]
    fn an_older_cookie_does_not_roll_back_a_refreshed_uid() {
        // Arrange: the cookie's UID expires at 5000; a refresh stored one
        // living until 6000.
        let coordinator = coordinator_with(RegisteredProvider::new(
            provider_config(""),
            RefreshingCookieProvider(cookie_provider(SourceInput::Anonymous, Some("vendor_id"))),
        ));
        let transport = CannedTransport::new(ok_reply);

        // Act
        let results = run(
            &coordinator,
            &transport,
            &snapshot_holding(record_until("p-2", 6000)),
            &granting(),
        );

        // Assert
        assert_eq!(
            only_status(&results),
            LookupStatus::Skipped(SkipReason::NotDue),
            "should keep the refreshed UID over the older cookie"
        );
        assert!(transport.sent.borrow().is_empty(), "should send nothing");
    }

    #[test]
    fn a_newer_cookie_replaces_an_older_stored_uid() {
        let coordinator = coordinator_with(RegisteredProvider::new(
            provider_config(""),
            RefreshingCookieProvider(cookie_provider(SourceInput::Anonymous, Some("vendor_id"))),
        ));
        let transport = CannedTransport::new(ok_reply);

        let results = run(
            &coordinator,
            &transport,
            &snapshot_holding(record_until("p-2", 4000)),
            &granting(),
        );

        assert!(
            matches!(only_status(&results), LookupStatus::Found { kept: 1, .. }),
            "should take the cookie UID that outlives the stored one"
        );
    }

    #[test]
    fn a_cookie_changing_only_the_agent_type_is_rewritten() {
        let coordinator = cookie_coordinator();
        let transport = CannedTransport::new(ok_reply);
        let uid = KvUid::new(
            UidValue::try_from("c-1").expect("should build uid"),
            AgentType::try_from(1).expect("should build atype"),
            None,
            Some(ProviderId::new("example-provider")),
            Expiry::Never,
        );
        let record = KvPartnerId::from_eids(
            vec![KvEid::new(EidFields::default(), vec![uid])],
            UnixSeconds::new(1000),
        )
        .expect("should build provider record");

        let results = run(
            &coordinator,
            &transport,
            &snapshot_holding(record),
            &granting(),
        );

        assert!(
            matches!(only_status(&results), LookupStatus::Found { kept: 1, .. }),
            "should store a cookie UID whose agent type changed"
        );
    }

    /// A two-source provider whose cookie source is excluded where GDPR
    /// applies.
    struct CookieSourceExcluded(TwoSourceProvider);

    impl IdentityProvider for CookieSourceExcluded {
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

        fn excluded_where_gdpr_applies(&self, source: &SourceDomain) -> bool {
            source.as_str() == "ids.example.com"
        }
    }

    #[test]
    fn an_excluded_source_is_neither_collected_nor_kept_where_gdpr_applies() {
        // Arrange
        let coordinator = two_source_coordinator(CookieSourceExcluded);
        let transport = CannedTransport::new(ok_reply);

        // Act
        let results = run(&coordinator, &transport, &live_row(), &tcf_consent(vec![]));

        // Assert
        let lookup = results.iter().next().expect("should report the provider");
        let sources: Vec<&str> = lookup
            .entries
            .iter()
            .map(|(source, _)| source.as_str())
            .collect();
        assert_eq!(
            sources,
            [SECOND_SOURCE],
            "should keep only the source that is not excluded"
        );
        assert_eq!(
            lookup.cleared,
            [SourceDomain::try_from("ids.example.com").expect("should parse")],
            "should remove the excluded source from the row"
        );
    }
}
