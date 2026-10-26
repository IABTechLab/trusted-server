//! Shared helpers for identity tests.

#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the test-utils library build compiles the crate-private helpers without their unit-test callers"
    )
)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use error_stack::Report;
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use url::Url;

use crate::consent::ConsentContext;
use crate::consent::jurisdiction::Jurisdiction;
use crate::consent::types::TcfConsent;
use crate::ec::identity::catalog::ProviderCatalog;
use crate::ec::identity::coordinator::{OutcomeContext, RequestFacts};
use crate::ec::identity::descriptor::{
    Captures, ConsentPolicy, EidProvenance, Input, ProviderDescriptor, ResolveKind, Resolves,
};
use crate::ec::identity::exchange::{ProviderRequest, ProviderResponse};
use crate::ec::identity::ids::{IdentityProviderId, SourceDomain, UnixTime};
use crate::ec::identity::input::{
    HashedEmail, ProviderConsent, Region, ResolveInput, ResolveSignals, SeededRandom, StoredId,
};
use crate::ec::identity::policy::PolicySignals;
use crate::ec::identity::provider::IdentityProvider;
use crate::ec::identity::resolution::{IdentityProviderError, Resolution, When};
use crate::ec::identity::revision::{StateRevision, new_revision};
use crate::ec::kv_backend::in_memory::InMemoryEcKv;
use crate::ec::kv_backend::{EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome};
use crate::ec::kv_types::{KvEid, KvPartnerId, KvUid};
use crate::ec::rate_limiter::RateLimiter;
use crate::error::TrustedServerError;
use crate::redacted::Redacted;
use crate::settings::{EcPartner, IdentitySettings, ProviderSettings};

/// A request with no consent signal and no regulated jurisdiction.
#[must_use]
pub fn test_consent_allowing_everything() -> ConsentContext {
    ConsentContext {
        jurisdiction: Jurisdiction::NonRegulated,
        ..ConsentContext::default()
    }
}

/// A GDPR request whose TCF string grants every purpose but not `vendor_id`.
#[must_use]
pub fn test_consent_gdpr_without_vendor(vendor_id: u16) -> ConsentContext {
    let vendors: Vec<u16> = (1..=20).filter(|vendor| *vendor != vendor_id).collect();
    ConsentContext {
        gdpr_applies: true,
        jurisdiction: Jurisdiction::Gdpr,
        tcf: Some(TcfConsent {
            version: 2,
            cmp_id: 1,
            cmp_version: 1,
            consent_screen: 1,
            consent_language: "EN".to_owned(),
            vendor_list_version: 1,
            tcf_policy_version: 4,
            created_ds: 0,
            last_updated_ds: 0,
            purpose_consents: vec![true; 24],
            purpose_legitimate_interests: vec![false; 24],
            vendor_consents: vendors,
            vendor_legitimate_interests: Vec::new(),
            special_feature_opt_ins: vec![false; 12],
        }),
        ..ConsentContext::default()
    }
}

/// Spec 6.1's example set: two entries, three UIDs, with UID-level and
/// EID-level fields.
#[must_use]
pub fn example_entry_set() -> Vec<KvEid> {
    vec![
        KvEid {
            uids: vec![
                KvUid {
                    id: "example-a".to_owned(),
                    atype: Some(1),
                    ext: None,
                },
                KvUid {
                    id: "example-b".to_owned(),
                    atype: Some(3),
                    ext: Some(json!({ "stype": "ppuid" })),
                },
            ],
            inserter: Some("example.com".to_owned()),
            mm: Some(3),
            ..KvEid::default()
        },
        KvEid {
            uids: vec![KvUid {
                id: "example-c".to_owned(),
                atype: Some(1),
                ext: None,
            }],
            matcher: Some("partner.example.com".to_owned()),
            ..KvEid::default()
        },
    ]
}

/// A record holding `set`, written by `writer`.
#[must_use]
pub fn full_record(writer: Option<&str>, set: Vec<KvEid>) -> KvPartnerId {
    KvPartnerId {
        w: writer.map(str::to_owned),
        eids: set,
        ..KvPartnerId::default()
    }
}

/// A descriptor-only resolver for `id`, declaring `sources`, with a dummy
/// endpoint and the `eid_persistence_only` policy.
///
/// # Panics
///
/// Panics when `id` or a source is not valid.
#[must_use]
pub fn fixed_provider(id: &str, sources: &[&str]) -> Arc<dyn IdentityProvider> {
    let mut builder =
        ProviderDescriptor::builder(id.parse().expect("should parse the provider ID"))
            .with_resolves(Resolves::new().with_endpoint("https://api.example.com"))
            .with_consent_policy(ConsentPolicy::eid_persistence_only());
    for source in sources {
        builder = builder.with_source(source.parse().expect("should parse the source"));
    }
    Arc::new(DescriptorOnly(
        builder.build().expect("should build the descriptor"),
    ))
}

/// A descriptor-only capture-only provider for `id`, declaring `sources`.
///
/// # Panics
///
/// Panics when `id` or a source is not valid.
#[must_use]
pub fn fixed_capture_provider(id: &str, sources: &[&str]) -> Arc<dyn IdentityProvider> {
    let mut builder =
        ProviderDescriptor::builder(id.parse().expect("should parse the provider ID"))
            .with_captures(Captures::new().with_path_prefix(&format!("/integrations/{id}/")))
            .with_consent_policy(ConsentPolicy::eid_persistence_only());
    for source in sources {
        builder = builder.with_source(source.parse().expect("should parse the source"));
    }
    Arc::new(DescriptorOnly(
        builder.build().expect("should build the descriptor"),
    ))
}

/// A [`fixed_provider`] that also declares a `with_provenance` default per
/// listed source.
///
/// # Panics
///
/// Panics when `id` or a source is not valid.
#[must_use]
pub fn provenance_provider(
    id: &str,
    sources: &[&str],
    provenance: Vec<(&str, EidProvenance)>,
) -> Arc<dyn IdentityProvider> {
    let mut builder =
        ProviderDescriptor::builder(id.parse().expect("should parse the provider ID"))
            .with_resolves(Resolves::new().with_endpoint("https://api.example.com"))
            .with_consent_policy(ConsentPolicy::eid_persistence_only());
    for source in sources {
        builder = builder.with_source(source.parse().expect("should parse the source"));
    }
    for (source, default) in provenance {
        builder =
            builder.with_provenance(&source.parse().expect("should parse the source"), default);
    }
    provider_from(builder.build().expect("should build the descriptor"))
}

/// A descriptor-only provider for `descriptor`, for tests that need more
/// than [`fixed_provider`] declares.
#[must_use]
pub fn provider_from(descriptor: ProviderDescriptor) -> Arc<dyn IdentityProvider> {
    Arc::new(DescriptorOnly(descriptor))
}

/// A `fixed_provider` declared `with_private_state()`.
///
/// # Panics
///
/// Panics when `id` or a source is not valid.
#[must_use]
pub fn stateful_fixed_provider(id: &str, sources: &[&str]) -> Arc<dyn IdentityProvider> {
    let mut builder =
        ProviderDescriptor::builder(id.parse().expect("should parse the provider ID"))
            .with_resolves(
                Resolves::new()
                    .with_endpoint("https://api.example.com")
                    .with_private_state(),
            )
            .with_consent_policy(ConsentPolicy::eid_persistence_only());
    for source in sources {
        builder = builder.with_source(source.parse().expect("should parse the source"));
    }
    provider_from(builder.build().expect("should build the descriptor"))
}

/// A request with no consent signal and no regulated jurisdiction.
#[must_use]
pub fn allowing_consent() -> ConsentContext {
    test_consent_allowing_everything()
}

/// A source domain.
///
/// # Panics
///
/// Panics when `domain` is not valid.
#[must_use]
pub fn source(domain: &str) -> SourceDomain {
    domain.parse().expect("should parse the source")
}

/// A provider ID.
///
/// # Panics
///
/// Panics when `id` is not valid.
#[must_use]
pub fn pid(id: &str) -> IdentityProviderId {
    id.parse().expect("should parse the provider ID")
}

/// A one-UID record with a fresh `rv`, written by `writer` at `at`.
#[must_use]
pub fn record_for_test(uid: &str, writer: Option<&str>, at: u64) -> KvPartnerId {
    KvPartnerId {
        w: writer.map(str::to_owned),
        rv: Some(new_revision(&SeededRandom::new(at)).as_str().to_owned()),
        at: Some(at),
        ..KvPartnerId::new(uid)
    }
}

/// Facts for a returning visitor's writer navigation that runs no auction:
/// a real browser, persistence allowed and every input available.
#[must_use]
pub(crate) fn facts_for_writer() -> RequestFacts {
    RequestFacts {
        is_real_browser: true,
        ec_persistence_allowed: true,
        policy_signals: PolicySignals::default(),
        writer: true,
        new_visitor: false,
        runs_auction: false,
        now: UnixTime::from_secs(1_000),
        available_inputs: BTreeSet::from([
            Input::ClientIp,
            Input::UserAgent,
            Input::PageUrl,
            Input::Region,
            Input::HashedEmail,
        ]),
    }
}

/// An owned fixture for [`OutcomeContext`], which borrows from it.
///
/// `new` holds a catalog with provider `p` claiming `a.example.com` and
/// `b.example.com`, no source owners and default settings.
pub(crate) struct OutcomeFixture {
    catalog: ProviderCatalog,
    settings: ProviderSettings,
    identity: IdentitySettings,
    expected_state: Option<StateRevision>,
    fingerprint: String,
}

impl OutcomeFixture {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            catalog: ProviderCatalog::new(
                vec![fixed_provider("p", &["a.example.com", "b.example.com"])],
                std::collections::BTreeMap::new(),
            ),
            settings: ProviderSettings::default(),
            identity: IdentitySettings::default(),
            expected_state: None,
            fingerprint: "0".to_owned(),
        }
    }

    /// Replaces the catalog.
    #[must_use]
    pub(crate) fn with_catalog(mut self, catalog: ProviderCatalog) -> Self {
        self.catalog = catalog;
        self
    }

    /// Replaces the provider's settings.
    #[must_use]
    pub(crate) fn with_settings(mut self, settings: ProviderSettings) -> Self {
        self.settings = settings;
        self
    }

    /// Sets the consent fingerprint an opt-out records.
    #[must_use]
    pub(crate) fn with_fingerprint(mut self, fingerprint: &str) -> Self {
        fingerprint.clone_into(&mut self.fingerprint);
        self
    }

    /// Sets the state revision the call read.
    #[must_use]
    pub(crate) fn with_expected_state(mut self, expected_state: StateRevision) -> Self {
        self.expected_state = Some(expected_state);
        self
    }

    /// The context for an outcome at `now`.
    #[must_use]
    pub(crate) fn context_at(&self, now: u64) -> OutcomeContext<'_> {
        OutcomeContext {
            now: UnixTime::from_secs(now),
            catalog: &self.catalog,
            settings: &self.settings,
            identity: &self.identity,
            expected_state: self.expected_state.clone(),
            fingerprint: &self.fingerprint,
        }
    }
}

/// A valid EC ID in `{64-hex}.{6-alnum}` format.
pub const TEST_EC_ID: &str =
    "0000000000000000000000000000000000000000000000000000000000000000.ABC123";

/// A partner with pull sync enabled, calling `pull_sync_url` with `token`.
///
/// The URL's host is the only allowed domain.
///
/// # Panics
///
/// Panics when `source` is not a domain or `pull_sync_url` is not a URL with a host.
#[must_use]
pub fn ec_partner(source: &str, pull_sync_url: &str, token: &str) -> EcPartner {
    let host = Url::parse(pull_sync_url)
        .expect("should parse the pull URL")
        .host_str()
        .expect("should have a pull URL host")
        .to_owned();
    EcPartner {
        name: format!("Partner {source}"),
        source_domain: source.to_owned(),
        openrtb_atype: EcPartner::default_openrtb_atype(),
        bidstream_enabled: true,
        api_token: None,
        batch_rate_limit: EcPartner::default_batch_rate_limit(),
        pull_sync_enabled: true,
        pull_sync_url: Some(pull_sync_url.to_owned()),
        pull_sync_allowed_domains: vec![host],
        removed_pull_sync_ttl_sec: None,
        removed_pull_sync_rate_limit: None,
        owner: None,
        ts_pull_token: Some(Redacted::new(token.to_owned())),
    }
}

/// A stored one-UID record of `source`, as a provider reads it.
///
/// # Panics
///
/// Panics when the record is not valid.
#[must_use]
pub(crate) fn stored_for_test(source: &SourceDomain, uid: &str) -> StoredId {
    StoredId::from_record(source, &KvPartnerId::new(uid))
        .expect("should build a stored ID from a record with a UID")
}

/// Write and lookup counts of a [`WriteCountingEcKv`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteCounts {
    /// Writes in [`EcKvWriteMode::Add`] mode.
    pub add: usize,
    /// Writes in [`EcKvWriteMode::IfGenerationMatch`] mode.
    pub if_generation_match: usize,
    /// Writes in [`EcKvWriteMode::Overwrite`] mode.
    pub overwrite: usize,
    /// Point reads.
    pub lookups: usize,
}

impl WriteCounts {
    /// Every write, whatever its mode.
    #[must_use]
    pub fn total(&self) -> usize {
        self.add + self.if_generation_match + self.overwrite
    }
}

/// An in-memory KV store that counts lookups and writes by mode.
///
/// Every write is applied or refused as the in-memory store does, so a
/// write that loses its precondition is still counted and reports
/// [`EcKvWriteOutcome::PreconditionFailed`].
pub struct WriteCountingEcKv {
    inner: InMemoryEcKv,
    add: AtomicUsize,
    if_generation_match: AtomicUsize,
    overwrite: AtomicUsize,
    lookups: AtomicUsize,
    refusals: Mutex<BTreeMap<String, usize>>,
}

impl WriteCountingEcKv {
    /// An empty store named `name`.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            inner: InMemoryEcKv::new(name),
            add: AtomicUsize::new(0),
            if_generation_match: AtomicUsize::new(0),
            overwrite: AtomicUsize::new(0),
            lookups: AtomicUsize::new(0),
            refusals: Mutex::new(BTreeMap::new()),
        }
    }

    /// Refuses the next `count` generation-checked writes to `key` with
    /// [`EcKvWriteOutcome::PreconditionFailed`], the retryable refusal. A
    /// refused write is still counted.
    ///
    /// # Panics
    ///
    /// Panics when the refusal table is poisoned.
    pub fn refuse_conditional_writes(&self, key: &str, count: usize) {
        self.refusals
            .lock()
            .expect("should lock the refusals")
            .insert(key.to_owned(), count);
    }

    /// The counts so far, seeding included.
    #[must_use]
    pub fn writes(&self) -> WriteCounts {
        WriteCounts {
            add: self.add.load(Ordering::SeqCst),
            if_generation_match: self.if_generation_match.load(Ordering::SeqCst),
            overwrite: self.overwrite.load(Ordering::SeqCst),
            lookups: self.lookups.load(Ordering::SeqCst),
        }
    }

    /// Zeroes the counts, to count only what follows the seeding.
    pub fn reset_counts(&self) {
        for count in [
            &self.add,
            &self.if_generation_match,
            &self.overwrite,
            &self.lookups,
        ] {
            count.store(0, Ordering::SeqCst);
        }
    }
}

impl EcKvStore for WriteCountingEcKv {
    fn store_name(&self) -> &str {
        self.inner.store_name()
    }

    fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        self.inner.lookup(key)
    }

    fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
        self.inner.key_exists(key)
    }

    fn insert(
        &self,
        key: &str,
        write: EcKvWrite<'_>,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
        let count = match write.mode {
            EcKvWriteMode::Add => &self.add,
            EcKvWriteMode::IfGenerationMatch(_) => &self.if_generation_match,
            EcKvWriteMode::Overwrite => &self.overwrite,
        };
        count.fetch_add(1, Ordering::SeqCst);
        if matches!(write.mode, EcKvWriteMode::IfGenerationMatch(_)) {
            let mut refusals = self.refusals.lock().expect("should lock the refusals");
            if let Some(remaining) = refusals.get_mut(key)
                && *remaining > 0
            {
                *remaining -= 1;
                return Ok(EcKvWriteOutcome::PreconditionFailed);
            }
        }
        self.inner.insert(key, write)
    }

    fn list_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<String>, Report<TrustedServerError>> {
        self.inner.list_keys_with_prefix(prefix, limit)
    }

    fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
        self.inner.delete(key)
    }
}

/// Drives one provider the way the coordinator does, so a vendor's tests
/// stay short.
///
/// A source with a [`with_stored`](Self::with_stored) record is a `Refresh`,
/// any other an `Initial`. Sources whose kind needs an input the harness
/// does not hold are left out, and with none left
/// [`request`](Self::request) returns `Ok(None)` without calling the
/// provider.
pub struct ProviderHarness<'provider> {
    provider: &'provider dyn IdentityProvider,
    client_ip: Option<IpAddr>,
    user_agent: Option<String>,
    page_url: Option<Url>,
    region: Option<Region>,
    hashed_email: Option<HashedEmail>,
    stored: BTreeMap<SourceDomain, StoredId>,
    state_json: Option<String>,
    now: UnixTime,
    random: SeededRandom,
    ec_id: Option<String>,
    last_request: Option<ProviderRequest>,
}

impl<'provider> ProviderHarness<'provider> {
    /// A harness for `provider` with no inputs, the clock at 1,760,000,000
    /// and random seed 1.
    #[must_use]
    pub fn new(provider: &'provider dyn IdentityProvider) -> Self {
        Self {
            provider,
            client_ip: None,
            user_agent: None,
            page_url: None,
            region: None,
            hashed_email: None,
            stored: BTreeMap::new(),
            state_json: None,
            now: UnixTime::from_secs(1_760_000_000),
            random: SeededRandom::new(1),
            ec_id: None,
            last_request: None,
        }
    }

    /// Sets the client IP.
    ///
    /// # Panics
    ///
    /// Panics when `ip` is not an IP address.
    #[must_use]
    pub fn with_client_ip(mut self, ip: &str) -> Self {
        self.client_ip = Some(ip.parse().expect("should parse the client IP"));
        self
    }

    /// Sets the `User-Agent`.
    #[must_use]
    pub fn with_user_agent(mut self, user_agent: &str) -> Self {
        self.user_agent = Some(user_agent.to_owned());
        self
    }

    /// Sets the page URL.
    ///
    /// # Panics
    ///
    /// Panics when `url` is not a URL.
    #[must_use]
    pub fn with_page_url(mut self, url: &str) -> Self {
        self.page_url = Some(Url::parse(url).expect("should parse the page URL"));
        self
    }

    /// Sets the region.
    ///
    /// # Panics
    ///
    /// Panics when `country` is not a country code.
    #[must_use]
    pub fn with_region(mut self, country: &str, subdivision: Option<&str>) -> Self {
        self.region =
            Some(Region::from_geo(country, subdivision).expect("should accept the country"));
        self
    }

    /// Sets the hashed email.
    #[must_use]
    pub fn with_hashed_email(mut self, email: HashedEmail) -> Self {
        self.hashed_email = Some(email);
        self
    }

    /// Gives the provider a stored record, valid or expired. Its source is
    /// the one its first entry names, or the provider's primary source.
    #[must_use]
    pub fn with_stored(mut self, stored: StoredId) -> Self {
        let source = stored
            .eids()
            .first()
            .and_then(|eid| eid.source.parse().ok())
            .unwrap_or_else(|| self.provider.descriptor().primary_source().clone());
        self.stored.insert(source, stored);
        self
    }

    /// Gives the provider its private state, as stored.
    ///
    /// # Panics
    ///
    /// Panics when `state` does not serialize.
    #[must_use]
    pub fn with_state(mut self, state: &impl Serialize) -> Self {
        self.state_json = Some(serde_json::to_string(state).expect("should serialize the state"));
        self
    }

    /// Sets the fixed clock.
    #[must_use]
    pub fn with_now(mut self, now: UnixTime) -> Self {
        self.now = now;
        self
    }

    /// Reseeds the random source.
    #[must_use]
    pub fn with_random_seed(mut self, seed: u64) -> Self {
        self.random = SeededRandom::new(seed);
        self
    }

    /// Gives the provider the EC ID. Only generic pull reads it.
    #[must_use]
    pub(crate) fn with_ec_id(mut self, ec_id: &str) -> Self {
        self.ec_id = Some(ec_id.to_owned());
        self
    }

    /// Calls `request` and keeps the built request and its call context.
    ///
    /// # Errors
    ///
    /// Returns what the provider returns.
    pub fn request(&mut self) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let kinds = self.kinds();
        let request = if kinds.is_empty() {
            None
        } else {
            let signals = self.signals();
            let input = ResolveInput::new(self.provider.descriptor(), kinds, &signals)
                .with_state_json(self.state_json.clone());
            self.provider.request(&input)?
        };
        self.last_request.clone_from(&request);
        Ok(request)
    }

    /// Decodes the last built request's JSON body.
    ///
    /// # Panics
    ///
    /// Panics when no request was built or its body is not a `T`.
    #[must_use]
    pub fn request_json<T: DeserializeOwned>(&self) -> T {
        let request = self
            .last_request
            .as_ref()
            .expect("should have built a request");
        serde_json::from_slice(request.body()).expect("should decode the request body")
    }

    /// Calls `response` with the kept call context.
    ///
    /// # Errors
    ///
    /// Returns what the provider returns.
    ///
    /// # Panics
    ///
    /// Panics when `body` does not serialize.
    pub fn respond(
        &mut self,
        status: StatusCode,
        body: &serde_json::Value,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let bytes = serde_json::to_vec(body).expect("should serialize the response body");
        self.respond_raw(status, &[], &bytes)
    }

    /// Calls `response` with `headers` and a raw `body`, such as an empty 204.
    ///
    /// # Errors
    ///
    /// Returns what the provider returns.
    ///
    /// # Panics
    ///
    /// Panics when a header is not valid.
    pub fn respond_raw(
        &mut self,
        status: StatusCode,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let mut header_map = HeaderMap::new();
        for (name, value) in headers {
            header_map.insert(
                HeaderName::from_bytes(name.as_bytes()).expect("should be a header name"),
                HeaderValue::from_str(value).expect("should be a header value"),
            );
        }
        let call_context = self
            .last_request
            .as_ref()
            .and_then(|request| request.call_context_json().map(str::to_owned));
        let response = ProviderResponse::new(status, header_map, body.to_vec(), call_context);
        let signals = self.signals();
        let input = ResolveInput::new(self.provider.descriptor(), self.kinds(), &signals)
            .with_state_json(self.state_json.clone());
        self.provider.response(&response, &input)
    }

    /// Resolves an expiry or refresh against the fixed clock.
    #[must_use]
    pub fn resolve(&self, when: When) -> UnixTime {
        when.resolve(self.now)
    }

    fn available_inputs(&self) -> BTreeSet<Input> {
        let mut available = BTreeSet::new();
        for (input, present) in [
            (Input::ClientIp, self.client_ip.is_some()),
            (Input::UserAgent, self.user_agent.is_some()),
            (Input::PageUrl, self.page_url.is_some()),
            (Input::Region, self.region.is_some()),
            (Input::HashedEmail, self.hashed_email.is_some()),
        ] {
            if present {
                available.insert(input);
            }
        }
        available
    }

    fn kinds(&self) -> BTreeMap<SourceDomain, ResolveKind> {
        let descriptor = self.provider.descriptor();
        let Some(resolves) = descriptor.resolves() else {
            return BTreeMap::new();
        };
        let available = self.available_inputs();
        descriptor
            .sources()
            .iter()
            .filter_map(|source| {
                let kind = if self.stored.contains_key(source) {
                    ResolveKind::Refresh
                } else {
                    ResolveKind::Initial
                };
                (resolves.kinds().contains(&kind) && resolves.needs(kind).is_subset(&available))
                    .then(|| (source.clone(), kind))
            })
            .collect()
    }

    fn signals(&self) -> ResolveSignals<'_> {
        ResolveSignals {
            client_ip: self.client_ip,
            user_agent: self.user_agent.as_deref(),
            page_url: self.page_url.clone(),
            region: self.region.clone(),
            hashed_email: self.hashed_email.clone(),
            consent: ProviderConsent::default(),
            publisher_domain: "www.example.com",
            now: self.now,
            random: &self.random,
            ec_id: self.ec_id.clone(),
            stored: self.stored.clone(),
        }
    }
}

struct DescriptorOnly(ProviderDescriptor);

impl IdentityProvider for DescriptorOnly {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.0
    }
}

/// An in-memory [`RateLimiter`] that can be set to fail each method.
///
/// `exceeded` counts like the Fastly limiter: it increments on every check
/// under the limit. `record` and `count` use the same counts.
#[derive(Debug, Default)]
pub struct FakeRateLimiter {
    counts: Mutex<HashMap<String, u32>>,
    fail_exceeded: AtomicBool,
    fail_record: AtomicBool,
    fail_count: AtomicBool,
}

#[allow(clippy::missing_panics_doc, reason = "the fake locks its own mutex")]
impl FakeRateLimiter {
    /// An empty limiter whose methods all succeed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes `exceeded` and `exceeded_per_minute` fail.
    #[must_use]
    pub fn failing_exceeded(self) -> Self {
        self.fail_exceeded.store(true, Ordering::SeqCst);
        self
    }

    /// Makes `record` fail.
    #[must_use]
    pub fn failing_record(self) -> Self {
        self.fail_record.store(true, Ordering::SeqCst);
        self
    }

    /// Makes `count` fail.
    #[must_use]
    pub fn failing_count(self) -> Self {
        self.fail_count.store(true, Ordering::SeqCst);
        self
    }

    /// Sets the count of `key`.
    #[must_use]
    pub fn with_count(self, key: &str, count: u32) -> Self {
        self.counts
            .lock()
            .expect("should lock the counts")
            .insert(key.to_owned(), count);
        self
    }

    /// The count of `key`, 0 when it was never touched.
    #[must_use]
    pub fn count_of(&self, key: &str) -> u32 {
        self.counts
            .lock()
            .expect("should lock the counts")
            .get(key)
            .copied()
            .unwrap_or(0)
    }

    /// Every key with a count, sorted.
    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .counts
            .lock()
            .expect("should lock the counts")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    fn unavailable() -> Report<TrustedServerError> {
        Report::new(TrustedServerError::Proxy {
            message: "limiter unavailable".to_owned(),
        })
    }
}

impl RateLimiter for FakeRateLimiter {
    fn exceeded(&self, key: &str, hourly_limit: u32) -> Result<bool, Report<TrustedServerError>> {
        if self.fail_exceeded.load(Ordering::SeqCst) {
            return Err(Self::unavailable());
        }
        let limit = hourly_limit.div_ceil(60);
        let mut counts = self.counts.lock().expect("should lock the counts");
        let current = counts.entry(key.to_owned()).or_insert(0);
        if *current >= limit {
            return Ok(true);
        }
        *current += 1;
        Ok(false)
    }

    fn record(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
        if self.fail_record.load(Ordering::SeqCst) {
            return Err(Self::unavailable());
        }
        *self
            .counts
            .lock()
            .expect("should lock the counts")
            .entry(key.to_owned())
            .or_insert(0) += 1;
        Ok(())
    }

    fn count(&self, key: &str) -> Result<u32, Report<TrustedServerError>> {
        if self.fail_count.load(Ordering::SeqCst) {
            return Err(Self::unavailable());
        }
        Ok(self.count_of(key))
    }
}

/// A fake HTTP client, backend and provider for dispatch tests.
#[cfg(any(test, feature = "test-utils"))]
#[allow(
    clippy::missing_panics_doc,
    clippy::new_without_default,
    reason = "test doubles lock their own mutexes and are built with new"
)]
pub mod fake_http {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use error_stack::Report;

    use crate::ec::identity::descriptor::EidProvenance;
    use crate::ec::identity::descriptor::{ConsentPolicy, ProviderDescriptor, Resolves};
    use crate::ec::identity::exchange::{ProviderRequest, ProviderResponse};
    use crate::ec::identity::ids::SourceDomain;
    use crate::ec::identity::input::ResolveInput;
    use crate::ec::identity::provider::IdentityProvider;
    use crate::ec::identity::resolution::{
        IdentityProviderError, Resolution, ResolvedEid, ResolvedUid,
    };
    #[cfg(test)]
    use crate::platform::test_support::{NoopConfigStore, NoopGeo, NoopSecretStore};
    use crate::platform::{
        BackendNamingPolicy, PlatformBackend, PlatformBackendSpec, PlatformError,
        PlatformHttpClient, PlatformHttpRequest, PlatformPendingRequest, PlatformResponse,
        PlatformSelectResult,
    };
    #[cfg(test)]
    use crate::platform::{ClientInfo, RuntimeServices};
    use serde_json::json;

    /// What the fake does for one host.
    #[derive(Clone)]
    enum Route {
        Respond {
            status: u16,
            headers: Vec<(String, String)>,
            body: Vec<u8>,
        },
        Fail,
    }

    struct FakePending {
        host: String,
        route: Route,
        delay: Duration,
    }

    /// An HTTP client that answers per host, and records what it was sent.
    pub struct FakeHttpClient {
        routes: Mutex<BTreeMap<String, Route>>,
        delays: Mutex<BTreeMap<String, Duration>>,
        sent: Mutex<Vec<String>>,
        uris: Mutex<Vec<String>>,
    }

    impl FakeHttpClient {
        /// A client with no routes.
        #[must_use]
        pub fn new() -> Self {
            Self {
                routes: Mutex::new(BTreeMap::new()),
                delays: Mutex::new(BTreeMap::new()),
                sent: Mutex::new(Vec::new()),
                uris: Mutex::new(Vec::new()),
            }
        }

        /// Answers `host` with `status` and `body`.
        pub fn respond(&self, host: &str, status: u16, body: &[u8]) {
            self.respond_with_headers(host, status, body, &[]);
        }

        /// Answers `host` with `status`, `headers` and `body`.
        pub fn respond_with_headers(
            &self,
            host: &str,
            status: u16,
            body: &[u8],
            headers: &[(&str, &str)],
        ) {
            let route = Route::Respond {
                status,
                headers: headers
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect(),
                body: body.to_vec(),
            };
            self.routes
                .lock()
                .expect("should lock the routes")
                .insert(host.to_owned(), route);
        }

        /// Makes the wait for `host` fail, as a first-byte timeout does.
        pub fn time_out(&self, host: &str) {
            self.routes
                .lock()
                .expect("should lock the routes")
                .insert(host.to_owned(), Route::Fail);
        }

        /// Makes the wait for `host` take `delay` of wall-clock time.
        pub fn delay(&self, host: &str, delay: Duration) {
            self.delays
                .lock()
                .expect("should lock the delays")
                .insert(host.to_owned(), delay);
        }

        /// The URIs requests were sent to, in order.
        pub fn sent_uris(&self) -> Vec<String> {
            self.uris.lock().expect("should lock the sent URIs").clone()
        }

        /// The hosts requests were sent to, in order.
        pub fn sent_hosts(&self) -> Vec<String> {
            self.sent
                .lock()
                .expect("should lock the sent hosts")
                .clone()
        }
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformHttpClient for FakeHttpClient {
        async fn send(
            &self,
            _request: PlatformHttpRequest,
        ) -> Result<PlatformResponse, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        async fn send_async(
            &self,
            request: PlatformHttpRequest,
        ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
            let host = request.request.uri().host().unwrap_or_default().to_owned();
            self.sent
                .lock()
                .expect("should lock the sent hosts")
                .push(host.clone());
            self.uris
                .lock()
                .expect("should lock the sent URIs")
                .push(request.request.uri().to_string());
            let route = self
                .routes
                .lock()
                .expect("should lock the routes")
                .get(&host)
                .cloned()
                .ok_or_else(|| Report::new(PlatformError::HttpClient))?;
            let delay = self
                .delays
                .lock()
                .expect("should lock the delays")
                .get(&host)
                .copied()
                .unwrap_or_default();
            Ok(PlatformPendingRequest::new(FakePending {
                host,
                route,
                delay,
            }))
        }

        async fn select(
            &self,
            mut pending_requests: Vec<PlatformPendingRequest>,
        ) -> Result<PlatformSelectResult, Report<PlatformError>> {
            if pending_requests.is_empty() {
                return Err(Report::new(PlatformError::HttpClient));
            }
            let first = pending_requests.remove(0);
            let ready = self.wait(first).await;
            Ok(PlatformSelectResult {
                ready,
                remaining: pending_requests,
                failed_backend_name: None,
            })
        }

        async fn wait(
            &self,
            pending: PlatformPendingRequest,
        ) -> Result<PlatformResponse, Report<PlatformError>> {
            let pending = pending
                .downcast::<FakePending>()
                .map_err(|_| Report::new(PlatformError::HttpClient))?;
            if !pending.delay.is_zero() {
                std::thread::sleep(pending.delay);
            }
            match pending.route {
                Route::Fail => Err(Report::new(PlatformError::HttpClient)),
                Route::Respond {
                    status,
                    headers,
                    body,
                } => {
                    let mut builder = edgezero_core::http::response_builder().status(status);
                    for (name, value) in headers {
                        builder = builder.header(name, value);
                    }
                    let response = builder
                        .body(edgezero_core::body::Body::from(body))
                        .map_err(|_| Report::new(PlatformError::HttpClient))?;
                    Ok(PlatformResponse::new(response).with_backend_name(pending.host))
                }
            }
        }
    }

    /// A backend named after its host, that records each spec it is given.
    pub struct FakeBackend {
        specs: Mutex<Vec<PlatformBackendSpec>>,
    }

    impl FakeBackend {
        /// A backend that has registered nothing.
        #[must_use]
        pub fn new() -> Self {
            Self {
                specs: Mutex::new(Vec::new()),
            }
        }

        /// The first-byte timeouts the backends were registered with, in order.
        pub fn first_byte_timeouts(&self) -> Vec<Duration> {
            self.specs
                .lock()
                .expect("should lock the specs")
                .iter()
                .map(|spec| spec.first_byte_timeout)
                .collect()
        }
    }

    impl PlatformBackend for FakeBackend {
        fn naming_policy(&self) -> BackendNamingPolicy {
            BackendNamingPolicy::Axum
        }

        fn predict_name(
            &self,
            spec: &PlatformBackendSpec,
        ) -> Result<String, Report<PlatformError>> {
            Ok(spec.host.clone())
        }

        fn ensure(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            self.specs
                .lock()
                .expect("should lock the specs")
                .push(spec.clone());
            Ok(spec.host.clone())
        }
    }

    /// Services that send through `client` and register backends in `backend`.
    #[cfg(test)]
    pub(crate) fn fake_services(
        client: &Arc<FakeHttpClient>,
        backend: &Arc<FakeBackend>,
    ) -> RuntimeServices {
        RuntimeServices::builder()
            .config_store(Arc::new(NoopConfigStore))
            .secret_store(Arc::new(NoopSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(backend.clone())
            .http_client(client.clone())
            .geo(Arc::new(NoopGeo))
            .client_info(ClientInfo::default())
            .build()
    }

    /// A resolver that calls its endpoint and answers `no_match` to a 2xx.
    pub(crate) struct ScriptedProvider {
        descriptor: ProviderDescriptor,
        reads_user_agent: bool,
        stateful: bool,
    }

    impl ScriptedProvider {
        /// A provider `id` calling `host`, claiming `source`. With `generic`
        /// it is a generic pull provider.
        pub(crate) fn new(id: &str, host: &str, source: &str, generic: bool) -> Self {
            let mut resolves = Resolves::new().with_endpoint(&format!("https://{host}/resolve"));
            if generic {
                resolves = resolves.with_ec_id();
            }
            let descriptor =
                ProviderDescriptor::builder(id.parse().expect("should parse the provider ID"))
                    .with_source(source.parse().expect("should parse the source"))
                    .with_resolves(resolves)
                    .with_consent_policy(ConsentPolicy::eid_persistence_only())
                    .build()
                    .expect("should build the descriptor");
            Self {
                descriptor,
                reads_user_agent: false,
                stateful: false,
            }
        }

        /// Reads `user_agent()` without declaring it, and ignores the error.
        pub(crate) fn reading_undeclared_user_agent(mut self) -> Self {
            self.reads_user_agent = true;
            self
        }

        /// Declares private state, and sends what it read in a `state` query.
        pub(crate) fn stateful(mut self) -> Self {
            self.stateful = true;
            self.descriptor = ProviderDescriptor::builder(self.descriptor.id().clone())
                .with_source(self.descriptor.primary_source().clone())
                .with_resolves(
                    Resolves::new()
                        .with_endpoint(
                            self.descriptor
                                .endpoint()
                                .expect("should have an endpoint")
                                .as_str(),
                        )
                        .with_private_state(),
                )
                .with_consent_policy(ConsentPolicy::eid_persistence_only())
                .build()
                .expect("should build the descriptor");
            self
        }

        pub(crate) fn into_provider(self) -> Arc<dyn IdentityProvider> {
            Arc::new(self)
        }
    }

    impl IdentityProvider for ScriptedProvider {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.descriptor
        }

        fn request(
            &self,
            input: &ResolveInput<'_>,
        ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
            if self.reads_user_agent {
                let _ = input.user_agent().ok();
            }
            let mut request = ProviderRequest::get(input.endpoint()?);
            if self.stateful {
                let state = input.state::<serde_json::Value>()?;
                request = request.with_query(
                    "state",
                    &state.map_or_else(|| "none".to_owned(), |state| state.to_string()),
                );
            }
            Ok(Some(request))
        }

        fn response(
            &self,
            response: &ProviderResponse,
            _input: &ResolveInput<'_>,
        ) -> Result<Resolution, Report<IdentityProviderError>> {
            response.success_text().map(|_| Resolution::no_match())
        }
    }

    /// A resolver that calls its endpoint and answers as the 2xx JSON body
    /// says: `{"answer":"found","entries":[{"uids":[{"id":"x","atype":1}],
    /// "inserter":"example.com"}]}`, `{"answer":"invalidate"}` or anything
    /// else for `no_match`.
    pub(crate) struct AnsweringProvider {
        descriptor: ProviderDescriptor,
        stateful: bool,
    }

    impl AnsweringProvider {
        /// A provider `id` calling `host`, claiming `source`, with `default`
        /// as its provenance default for the source when given.
        pub(crate) fn new(
            id: &str,
            host: &str,
            source: &str,
            default: Option<EidProvenance>,
        ) -> Self {
            Self::build(id, host, source, default, false)
        }

        /// As [`Self::new`], declaring private state that a `found` replaces.
        pub(crate) fn stateful(id: &str, host: &str, source: &str) -> Self {
            Self::build(id, host, source, None, true)
        }

        fn build(
            id: &str,
            host: &str,
            source: &str,
            default: Option<EidProvenance>,
            stateful: bool,
        ) -> Self {
            let mut resolves = Resolves::new().with_endpoint(&format!("https://{host}/resolve"));
            if stateful {
                resolves = resolves.with_private_state();
            }
            let source: SourceDomain = source.parse().expect("should parse the source");
            let mut builder =
                ProviderDescriptor::builder(id.parse().expect("should parse the provider ID"))
                    .with_source(source.clone())
                    .with_resolves(resolves)
                    .with_consent_policy(ConsentPolicy::eid_persistence_only());
            if let Some(default) = default {
                builder = builder.with_provenance(&source, default);
            }
            Self {
                descriptor: builder.build().expect("should build the descriptor"),
                stateful,
            }
        }

        pub(crate) fn into_provider(self) -> Arc<dyn IdentityProvider> {
            Arc::new(self)
        }
    }

    impl IdentityProvider for AnsweringProvider {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.descriptor
        }

        fn request(
            &self,
            input: &ResolveInput<'_>,
        ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
            if self.stateful {
                input.state::<serde_json::Value>()?;
            }
            Ok(Some(ProviderRequest::get(input.endpoint()?)))
        }

        fn response(
            &self,
            response: &ProviderResponse,
            _input: &ResolveInput<'_>,
        ) -> Result<Resolution, Report<IdentityProviderError>> {
            let body: serde_json::Value = response.success_json()?;
            let source = self.descriptor.primary_source();
            match body["answer"].as_str() {
                Some("found") => {
                    let eids = body["entries"]
                        .as_array()
                        .map(Vec::as_slice)
                        .unwrap_or_default()
                        .iter()
                        .map(|entry| {
                            let mut uids =
                                entry["uids"].as_array().into_iter().flatten().map(|uid| {
                                    let mut resolved = ResolvedUid::new(
                                        uid["id"].as_str().expect("should have a UID"),
                                    );
                                    if let Some(atype) = uid["atype"].as_i64() {
                                        resolved = resolved.with_atype(
                                            i32::try_from(atype).expect("should fit an atype"),
                                        );
                                    }
                                    resolved
                                });
                            let mut eid = ResolvedEid::new(
                                source,
                                uids.next().expect("should have a first UID"),
                            );
                            for uid in uids {
                                eid = eid.with_uid(uid);
                            }
                            if let Some(inserter) = entry["inserter"].as_str() {
                                eid = eid.with_inserter(inserter);
                            }
                            eid
                        });
                    let mut resolution = Resolution::found(eids);
                    if self.stateful {
                        resolution = resolution.replacing_state(&json!({ "seen": true }));
                    }
                    Ok(resolution)
                }
                Some("invalidate") => Ok(Resolution::invalidate(source)),
                _ => Ok(Resolution::no_match()),
            }
        }
    }
}

/// A returning writer navigation with a live row, hold-mode providers and a
/// fake client, for the tests of hold dispatch and the flush that follows.
#[cfg(test)]
pub(crate) mod hold_fixture {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use http::StatusCode;

    use super::fake_http::{FakeBackend, FakeHttpClient, fake_services};
    use super::{FakeRateLimiter, TEST_EC_ID, WriteCountingEcKv, allowing_consent};
    use crate::ec::EcContext;
    use crate::ec::identity::coordinator::{HoldResults, run_hold};
    use crate::ec::identity::flush::{AfterSendInput, FlushOutcome, run_after_send};
    use crate::ec::identity::ids::{IdentityProviderId, SourceDomain};
    use crate::ec::identity::input::ThreadRandom;
    use crate::ec::identity::provider::IdentityProvider;
    use crate::ec::identity::request::WriterDecision;
    use crate::ec::identity::runtime::IdentityRuntime;
    use crate::ec::kv::KvIdentityGraph;
    use crate::ec::kv_types::KvEntry;
    use crate::platform::RuntimeServices;
    use crate::settings::{ProviderMode, ProviderSettings, Settings};
    use crate::test_support::tests::create_test_settings;

    /// A live, consenting row sized for the flush, with no IDs.
    pub(crate) fn live_row() -> KvEntry {
        let mut entry = KvEntry::new(&allowing_consent(), None, 1_000, "test-publisher.com");
        entry.network = Some(crate::ec::kv_types::KvNetwork {
            cluster_size: Some(1),
        });
        entry
    }

    /// Everything one hold navigation reads.
    pub(crate) struct HoldFixture {
        pub(crate) settings: Settings,
        pub(crate) runtime: Arc<IdentityRuntime>,
        pub(crate) client: Arc<FakeHttpClient>,
        pub(crate) backend: Arc<FakeBackend>,
        pub(crate) services: RuntimeServices,
        pub(crate) graph: KvIdentityGraph,
        pub(crate) store: Arc<WriteCountingEcKv>,
        pub(crate) context: EcContext,
        pub(crate) limiter: Arc<FakeRateLimiter>,
    }

    impl HoldFixture {
        /// A writer navigation with `row` read in phase 1. Each provider runs
        /// in the given mode. With `runs_auction` the navigation runs one.
        pub(crate) fn new(
            providers: Vec<(Arc<dyn IdentityProvider>, ProviderMode)>,
            row: &KvEntry,
            runs_auction: bool,
        ) -> Self {
            let mut settings = create_test_settings();
            for (provider, mode) in &providers {
                settings.ec.identity.providers.insert(
                    provider.descriptor().id().to_string(),
                    ProviderSettings {
                        mode: *mode,
                        ..ProviderSettings::default()
                    },
                );
            }
            let runtime = Arc::new(
                IdentityRuntime::from_settings_with_providers(
                    &settings,
                    providers
                        .into_iter()
                        .map(|(provider, _)| provider)
                        .collect(),
                    BTreeMap::<SourceDomain, IdentityProviderId>::new(),
                )
                .expect("should build the runtime"),
            );
            let client = Arc::new(FakeHttpClient::new());
            let backend = Arc::new(FakeBackend::new());
            let services = fake_services(&client, &backend);
            let (graph, store) = KvIdentityGraph::write_counting("hold-store");
            graph.create(TEST_EC_ID, row).expect("should seed the row");
            let mut context = EcContext::new_for_test_with_ip(
                Some(TEST_EC_ID.to_owned()),
                allowing_consent(),
                Some("192.0.2.10".to_owned()),
            );
            context.set_identity_runtime(Arc::clone(&runtime));
            context.identity_mut().set_writer_decision(WriterDecision {
                writer: true,
                is_real_browser: true,
                user_agent: Some("ExampleBrowser/1.0".to_owned()),
                dnt: false,
                page_url: None,
            });
            context.identity_mut().set_runs_auction(runs_auction);
            context.set_kv_snapshot(graph.load_snapshot(TEST_EC_ID));
            store.reset_counts();
            Self {
                settings,
                runtime,
                client,
                backend,
                services,
                graph,
                store,
                context,
                limiter: Arc::new(FakeRateLimiter::new()),
            }
        }

        /// Runs the hold dispatch before the auction.
        pub(crate) fn run_hold(&mut self) -> HoldResults {
            let random = ThreadRandom;
            let deps = self.runtime.deps(
                &self.settings,
                &self.services,
                &random,
                Some(self.limiter.as_ref()),
            );
            run_hold(&deps, &mut self.context)
        }

        /// Runs phase 4 for a response with `status`.
        pub(crate) fn flush(&mut self, status: StatusCode) -> FlushOutcome {
            run_after_send(AfterSendInput {
                runtime: &self.runtime,
                settings: &self.settings,
                services: &self.services,
                graph: &self.graph,
                context: &mut self.context,
                status,
                rate_limiter: self.limiter.as_ref(),
            })
        }

        /// The row as KV holds it.
        pub(crate) fn row(&self) -> KvEntry {
            self.graph
                .get(TEST_EC_ID)
                .expect("should read the row")
                .expect("should hold the row")
                .0
        }
    }
}
