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
use std::time::Duration;

use error_stack::Report;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use url::Url;

use crate::consent::jurisdiction::Jurisdiction;
use crate::consent::types::RawConsentSignals;
use crate::consent::types::TcfConsent;
use crate::consent::{ConsentContext, build_context_from_signals};
use crate::ec::identity::catalog::ProviderCatalog;
use crate::ec::identity::coordinator::{OutcomeContext, RequestFacts};
use crate::ec::identity::descriptor::{
    Access, Captures, ConsentPolicy, EidProvenance, Input, ProviderDescriptor, ResolveKind,
    Resolves,
};
use crate::ec::identity::exchange::{ProviderRequest, ProviderResponse};
use crate::ec::identity::ids::{IdentityProviderId, SourceDomain, UnixTime};
use crate::ec::identity::input::{
    CaptureInput, HashedEmail, ProviderConsent, Region, ResolveInput, ResolveSignals, SeededRandom,
    StoredId,
};
use crate::ec::identity::policy::{PolicySignals, policy_allows};
use crate::ec::identity::provider::{IdentityProvider, IdentityProviderConfig};
use crate::ec::identity::resolution::{
    IdentityProviderError, Resolution, ResolvedEid, ResolvedUid, StateChange, When, entry_sets,
};
use crate::ec::identity::revision::{StateRevision, new_revision};
use crate::ec::kv_backend::in_memory::InMemoryEcKv;
use crate::ec::kv_backend::{EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome};
use crate::ec::kv_types::{KvEid, KvPartnerId, KvUid};
use crate::ec::rate_limiter::RateLimiter;
use crate::error::TrustedServerError;
use crate::openrtb::{Eid, Uid};
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

/// A capture-only provider that answers as the 2xx JSON body says:
/// `{"tokens":[{"source":"a.example.com","value":"x"}],"invalidate":["a.example.com"],
/// "opted_out":true,"state":"s"}`. Every member is optional. `opted_out` wins,
/// then `tokens`, then `invalidate`, and anything else is `no_match`.
struct AnsweringCapture {
    descriptor: ProviderDescriptor,
}

impl IdentityProvider for AnsweringCapture {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn capture(
        &self,
        input: &CaptureInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let Ok(body) = input.response().json::<serde_json::Value>() else {
            return Ok(Resolution::no_match());
        };
        let sources = |name: &str| -> Vec<SourceDomain> {
            body[name]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|value| value.as_str()?.parse().ok())
                .collect()
        };
        let tokens: Vec<ResolvedEid> = body["tokens"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|token| {
                let source = input.source_named(token["source"].as_str()?)?;
                Some(ResolvedEid::new(
                    source,
                    ResolvedUid::new(token["value"].as_str()?),
                ))
            })
            .collect();
        let invalidated = sources("invalidate");
        let mut resolution = if body["opted_out"].as_bool() == Some(true) {
            Resolution::opted_out()
        } else if !tokens.is_empty() {
            Resolution::found(tokens)
        } else if let Some(first) = invalidated.first() {
            Resolution::invalidate(first)
        } else {
            Resolution::no_match()
        };
        for source in &invalidated {
            resolution = resolution.also_invalidate(source);
        }
        if let Some(state) = body["state"].as_str() {
            resolution = resolution.replacing_state(&state);
        }
        Ok(resolution)
    }
}

/// A capture-only provider `id` over `sources` that answers as the response
/// body says, with a capture rule on `/integrations/<id>/`. With `gvl_id` its
/// consent policy names that TCF vendor.
///
/// # Panics
///
/// Panics when `id` or a source is not valid.
#[must_use]
pub fn answering_capture_provider(
    id: &str,
    sources: &[&str],
    gvl_id: Option<u16>,
) -> Arc<dyn IdentityProvider> {
    answering_capture_provider_at(id, sources, gvl_id, &format!("/integrations/{id}/"))
}

/// As [`answering_capture_provider`], with the capture rule on `prefix`.
///
/// # Panics
///
/// Panics when `id` or a source is not valid.
#[must_use]
pub fn answering_capture_provider_at(
    id: &str,
    sources: &[&str],
    gvl_id: Option<u16>,
    prefix: &str,
) -> Arc<dyn IdentityProvider> {
    let policy = match gvl_id {
        Some(vendor) => ConsentPolicy::strict().with_tcf_vendor(vendor),
        None => ConsentPolicy::eid_persistence_only(),
    };
    let mut builder =
        ProviderDescriptor::builder(id.parse().expect("should parse the provider ID"))
            .with_captures(Captures::new().with_path_prefix(prefix))
            .with_consent_policy(policy);
    for source in sources {
        builder = builder.with_source(source.parse().expect("should parse the source"));
    }
    Arc::new(AnsweringCapture {
        descriptor: builder.build().expect("should build the descriptor"),
    })
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

/// The source [`MultiEntryTestProvider`] writes.
pub const MULTI_ENTRY_SOURCE: &str = "exampleid.example.com";

/// The endpoint [`MultiEntryTestProvider`] calls.
pub const MULTI_ENTRY_ENDPOINT: &str = "https://api.exampleid.example.com/v1/resolve";

/// A resolver for [`MULTI_ENTRY_SOURCE`] that returns a complete set, for
/// request-level tests of a registered provider.
///
/// The JSON body of the endpoint's reply picks the answer through its
/// `entries` member:
///
/// - `"full"`, or a body without the member: two entries. The first holds
///   `uid-a` (`atype` 1, a UID-level `ext`) and `uid-b`, with `inserter`,
///   `matcher` and `mm`. The second holds `uid-c` and an EID-level `ext`, and
///   sets none of `inserter`, `matcher` and `mm`.
/// - `"first"`: the first entry alone.
/// - `"single"`: `uid-a` with `atype` 1 and its UID-level `ext`, and no
///   EID-level field.
/// - `"oversized"`: a set over `MAX_ENTRY_SET_BYTES`, which also invalidates
///   the source.
/// - `"none"`: no match.
///
/// The descriptor declares a `with_provenance` default of an `inserter` and a
/// `matcher`. [`stateful`](Self::stateful) is the same provider declared
/// `with_private_state()`; it keeps `{"seen": true}` in every answer that
/// finds an ID.
pub struct MultiEntryTestProvider {
    descriptor: ProviderDescriptor,
    stateful: bool,
}

impl MultiEntryTestProvider {
    /// The stateless provider.
    ///
    /// # Panics
    ///
    /// Panics when the fixed descriptor does not build.
    #[must_use]
    pub fn new() -> Self {
        Self::build(false)
    }

    /// The same provider, declared `with_private_state()`.
    ///
    /// # Panics
    ///
    /// Panics when the fixed descriptor does not build.
    #[must_use]
    pub fn stateful() -> Self {
        Self::build(true)
    }

    fn build(stateful: bool) -> Self {
        let mut resolves = Resolves::new().with_endpoint(MULTI_ENTRY_ENDPOINT);
        if stateful {
            resolves = resolves.with_private_state();
        }
        let descriptor =
            ProviderDescriptor::builder("exampleid".parse().expect("should parse the provider ID"))
                .with_source(source(MULTI_ENTRY_SOURCE))
                .with_resolves(resolves)
                .with_consent_policy(ConsentPolicy::strict())
                .with_provenance(
                    &source(MULTI_ENTRY_SOURCE),
                    EidProvenance::new()
                        .with_inserter("default-inserter.example.com")
                        .with_matcher("default-matcher.example.com"),
                )
                .build()
                .expect("should build the descriptor");
        Self {
            descriptor,
            stateful,
        }
    }

    fn first_entry(source: &SourceDomain) -> ResolvedEid {
        ResolvedEid::new(
            source,
            ResolvedUid::new("uid-a")
                .with_atype(1)
                .with_ext(json!({ "rtiPartner": "example" })),
        )
        .with_uid(ResolvedUid::new("uid-b"))
        .with_inserter("inserter-a.example.com")
        .with_matcher("matcher-a.example.com")
        .with_mm(2)
    }
}

impl Default for MultiEntryTestProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl IdentityProvider for MultiEntryTestProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        Ok(Some(ProviderRequest::get(input.endpoint()?)))
    }

    fn response(
        &self,
        response: &ProviderResponse,
        _input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let body: serde_json::Value = response.success_json()?;
        let source = source(MULTI_ENTRY_SOURCE);
        let resolution = match body["entries"].as_str() {
            Some("none") => Resolution::no_match(),
            Some("first") => Resolution::found([Self::first_entry(&source)]),
            Some("single") => Resolution::found([ResolvedEid::new(
                &source,
                ResolvedUid::new("uid-a")
                    .with_atype(1)
                    .with_ext(json!({ "rtiPartner": "example" })),
            )]),
            Some("oversized") => {
                let entries = (0..8).map(|index| {
                    ResolvedEid::new(
                        &source,
                        ResolvedUid::new(format!("uid-{index}"))
                            .with_ext(json!({ "pad": "x".repeat(1_000) })),
                    )
                });
                return Ok(self.with_state(Resolution::found(entries).also_invalidate(&source)));
            }
            _ => Resolution::found([
                Self::first_entry(&source),
                ResolvedEid::new(&source, ResolvedUid::new("uid-c"))
                    .with_ext(json!({ "segments": ["example"] })),
            ]),
        };
        Ok(self.with_state(resolution))
    }
}

impl MultiEntryTestProvider {
    fn with_state(&self, resolution: Resolution) -> Resolution {
        if self.stateful {
            resolution.replacing_state(&json!({ "seen": true }))
        } else {
            resolution
        }
    }
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
    StoredId::for_test(source, uid)
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
    /// Point reads of identity rows.
    pub lookups: usize,
    /// Point reads of pending keys (spec 5.3, Pending key).
    pub pending_lookups: usize,
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
    pending_lookups: AtomicUsize,
    refusals: Mutex<BTreeMap<String, usize>>,
    failing: Mutex<BTreeSet<String>>,
    failing_all: Mutex<BTreeSet<String>>,
    pauses: Mutex<Vec<Duration>>,
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
            pending_lookups: AtomicUsize::new(0),
            refusals: Mutex::new(BTreeMap::new()),
            failing: Mutex::new(BTreeSet::new()),
            failing_all: Mutex::new(BTreeSet::new()),
            pauses: Mutex::new(Vec::new()),
        }
    }

    /// Fails every generation-checked write to `key` with a store error that
    /// is never retried. A failed write is still counted.
    ///
    /// # Panics
    ///
    /// Panics when the failure table is poisoned.
    pub fn fail_conditional_writes(&self, key: &str) {
        self.failing
            .lock()
            .expect("should lock the failures")
            .insert(key.to_owned());
    }

    /// Fails every write to `key`, whatever its mode, with a store error.
    /// A failed write is still counted.
    ///
    /// # Panics
    ///
    /// Panics when the failure table is poisoned.
    pub fn fail_all_writes(&self, key: &str) {
        self.failing_all
            .lock()
            .expect("should lock the failures")
            .insert(key.to_owned());
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

    /// Every [`EcKvStore::pause`] so far, in order.
    ///
    /// # Panics
    ///
    /// Panics when the pause list is poisoned.
    #[must_use]
    pub fn pauses(&self) -> Vec<Duration> {
        self.pauses.lock().expect("should lock the pauses").clone()
    }

    /// The counts so far, seeding included.
    #[must_use]
    pub fn writes(&self) -> WriteCounts {
        WriteCounts {
            add: self.add.load(Ordering::SeqCst),
            if_generation_match: self.if_generation_match.load(Ordering::SeqCst),
            overwrite: self.overwrite.load(Ordering::SeqCst),
            lookups: self.lookups.load(Ordering::SeqCst),
            pending_lookups: self.pending_lookups.load(Ordering::SeqCst),
        }
    }

    /// Zeroes the counts, to count only what follows the seeding.
    pub fn reset_counts(&self) {
        for count in [
            &self.add,
            &self.if_generation_match,
            &self.overwrite,
            &self.lookups,
            &self.pending_lookups,
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
        let count = if key.starts_with("pending:") {
            &self.pending_lookups
        } else {
            &self.lookups
        };
        count.fetch_add(1, Ordering::SeqCst);
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
        if self
            .failing_all
            .lock()
            .expect("should lock the failures")
            .contains(key)
        {
            return Err(Report::new(TrustedServerError::KvStore {
                store_name: self.inner.store_name().to_owned(),
                message: "store unavailable".to_owned(),
            }));
        }
        if matches!(write.mode, EcKvWriteMode::IfGenerationMatch(_)) {
            if self
                .failing
                .lock()
                .expect("should lock the failures")
                .contains(key)
            {
                return Err(Report::new(TrustedServerError::KvStore {
                    store_name: self.inner.store_name().to_owned(),
                    message: "store unavailable".to_owned(),
                }));
            }
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

    fn pause(&self, duration: Duration) {
        self.pauses
            .lock()
            .expect("should lock the pauses")
            .push(duration);
    }
}

/// The harness clock when a test does not set one.
const HARNESS_NOW: u64 = 1_760_000_000;

/// Inputs for one harness run. The harness's own `with_*` methods delegate
/// to an inner `HarnessSetup`.
#[derive(Debug, Clone, Default)]
pub struct HarnessSetup {
    client_ip: Option<IpAddr>,
    user_agent: Option<String>,
    page_url: Option<Url>,
    region: Option<Region>,
    hashed_email: Option<HashedEmail>,
    consent: TestConsent,
    stored: Vec<StoredId>,
    state_json: Option<String>,
    now: Option<UnixTime>,
    random_seed: Option<u64>,
}

impl HarnessSetup {
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

    /// Sets the page URL, without its query string and fragment, as a
    /// request gives it to a provider.
    ///
    /// # Panics
    ///
    /// Panics when `url` is not a URL.
    #[must_use]
    pub fn with_page_url(mut self, url: &str) -> Self {
        let mut url = Url::parse(url).expect("should parse the page URL");
        url.set_query(None);
        url.set_fragment(None);
        self.page_url = Some(url);
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

    /// Sets the raw consent strings. Default: GDPR does not apply and there
    /// are no opt-outs.
    #[must_use]
    pub fn with_consent(mut self, consent: TestConsent) -> Self {
        self.consent = consent;
        self
    }

    /// Gives the provider a stored record, valid or expired. Its source is
    /// the one its first entry names, or the provider's primary source.
    #[must_use]
    pub fn with_stored(mut self, stored: StoredId) -> Self {
        self.stored.push(stored);
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

    /// Sets the fixed clock. Default: 1,760,000,000.
    #[must_use]
    pub fn with_now(mut self, now: UnixTime) -> Self {
        self.now = Some(now);
        self
    }

    /// Seeds the random source. Default: 1.
    #[must_use]
    pub fn with_random_seed(mut self, seed: u64) -> Self {
        self.random_seed = Some(seed);
        self
    }

    fn now(&self) -> UnixTime {
        self.now.unwrap_or(UnixTime::from_secs(HARNESS_NOW))
    }

    fn random(&self) -> SeededRandom {
        SeededRandom::new(self.random_seed.unwrap_or(1))
    }
}

/// Raw TCF, GPP and US Privacy strings, as a request would carry them.
#[derive(Debug, Clone, Default)]
pub struct TestConsent {
    gdpr_applies: Option<bool>,
    tc_string: Option<String>,
    gpp: Option<(String, Vec<u16>)>,
    us_privacy: Option<String>,
    gpc: bool,
    dnt: bool,
}

impl TestConsent {
    /// Sets whether GDPR applies, whatever the strings say.
    #[must_use]
    pub fn with_gdpr_applies(mut self, applies: bool) -> Self {
        self.gdpr_applies = Some(applies);
        self
    }

    /// Sets the raw TCF string.
    #[must_use]
    pub fn with_tc_string(mut self, tc: &str) -> Self {
        self.tc_string = Some(tc.to_owned());
        self
    }

    /// Sets the raw GPP string and its section IDs.
    #[must_use]
    pub fn with_gpp_string(mut self, gpp: &str, section_ids: &[u16]) -> Self {
        self.gpp = Some((gpp.to_owned(), section_ids.to_vec()));
        self
    }

    /// Sets the raw US Privacy string.
    #[must_use]
    pub fn with_us_privacy(mut self, us_privacy: &str) -> Self {
        self.us_privacy = Some(us_privacy.to_owned());
        self
    }

    /// Sets the Global Privacy Control signal.
    #[must_use]
    pub fn with_gpc(mut self) -> Self {
        self.gpc = true;
        self
    }

    /// Sets the `DNT: 1` signal.
    #[must_use]
    pub fn with_dnt(mut self) -> Self {
        self.dnt = true;
        self
    }

    /// The consent context a request carrying these signals would build.
    fn context(&self) -> ConsentContext {
        let signals = RawConsentSignals {
            raw_tc_string: self.tc_string.clone(),
            raw_gpp_string: self.gpp.as_ref().map(|(gpp, _)| gpp.clone()),
            raw_gpp_sid: self
                .gpp
                .as_ref()
                .map(|(_, ids)| ids.iter().map(u16::to_string).collect::<Vec<_>>().join(",")),
            raw_us_privacy: self.us_privacy.clone(),
            gpc: self.gpc,
        };
        let mut context = build_context_from_signals(&signals);
        // A known non-regulated region, so only the strings given matter.
        context.jurisdiction = Jurisdiction::NonRegulated;
        if let Some(applies) = self.gdpr_applies {
            context.gdpr_applies = applies;
        }
        context
    }
}

/// One proxied call: the request a capture rule matches, and its response.
#[derive(Debug, Clone)]
pub struct CaptureFixture {
    method: Method,
    path: String,
    status: StatusCode,
    body: Vec<u8>,
}

impl CaptureFixture {
    /// A proxied call with `method` and `path`, answered 200 with no body.
    #[must_use]
    pub fn new(method: Method, path: &str) -> Self {
        Self {
            method,
            path: path.to_owned(),
            status: StatusCode::OK,
            body: Vec::new(),
        }
    }

    /// Sets the response status and JSON body.
    ///
    /// # Panics
    ///
    /// Panics when `body` does not serialize.
    #[must_use]
    pub fn with_response(mut self, status: StatusCode, body: &serde_json::Value) -> Self {
        self.status = status;
        self.body = serde_json::to_vec(body).expect("should serialize the response body");
        self
    }
}

#[derive(Debug, Clone)]
struct ResolveCase {
    setup: HarnessSetup,
    status: StatusCode,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// The setups to run `request` under, the responses to answer with, and the
/// proxied calls to capture from, for one conformance run.
#[derive(Debug, Clone, Default)]
pub struct ConformanceCases {
    resolve: Vec<ResolveCase>,
    capture: Vec<CaptureFixture>,
}

impl ConformanceCases {
    /// A setup to run `request` under, and the response to answer it with.
    ///
    /// # Panics
    ///
    /// Panics when `response` does not serialize.
    #[must_use]
    pub fn with_resolve_case(
        self,
        setup: HarnessSetup,
        status: StatusCode,
        response: &serde_json::Value,
    ) -> Self {
        let body = serde_json::to_vec(response).expect("should serialize the response body");
        self.with_resolve_case_raw(setup, status, &[], &body)
    }

    /// As [`Self::with_resolve_case`], for text, binary or empty replies,
    /// such as a base64 body or a 204.
    #[must_use]
    pub fn with_resolve_case_raw(
        mut self,
        setup: HarnessSetup,
        status: StatusCode,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Self {
        self.resolve.push(ResolveCase {
            setup,
            status,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            body: body.to_vec(),
        });
        self
    }

    /// A proxied call to run `capture` on.
    #[must_use]
    pub fn with_capture_case(mut self, fixture: CaptureFixture) -> Self {
        self.capture.push(fixture);
        self
    }
}

/// The first conformance check a provider failed. Each variant names what
/// failed and never a value.
#[derive(Debug, derive_more::Display)]
#[non_exhaustive]
pub enum ConformanceFailure {
    /// A read of something the descriptor does not declare.
    #[display("the provider read `{access}` without declaring it")]
    UndeclaredInput {
        /// What was read.
        access: Access,
    },
    /// A request to a host the descriptor does not declare.
    #[display("a request went to a host the descriptor does not declare")]
    UndeclaredHost,
    /// A request that is not `https`.
    #[display("a request did not use https")]
    InsecureScheme,
    /// A request that sets a `Cookie` header.
    #[display("a request set a Cookie header")]
    CookieHeader,
    /// A request header whose value is not visible ASCII.
    #[display("a request header value is not valid")]
    InvalidHeader,
    /// An entry for a source the descriptor does not declare.
    #[display("a result holds an entry for undeclared source `{source}`")]
    UndeclaredSource {
        /// The source.
        source: SourceDomain,
    },
    /// A fixture result that hits a limit of spec 4.5.
    #[display("a result for `{source}` hit a limit: {part} was dropped")]
    LimitHit {
        /// The source.
        source: SourceDomain,
        /// What the limit dropped.
        part: String,
    },
    /// A fixture set over `MAX_ENTRY_SET_BYTES`.
    #[display("the entry set for `{source}` is over the size limit")]
    SetTooLarge {
        /// The source.
        source: SourceDomain,
    },
    /// A capability the descriptor declares and the provider does not
    /// override.
    #[display("`{method}` is declared but not implemented")]
    NotImplemented {
        /// The missing method.
        method: &'static str,
    },
    /// A provider that declares `resolves` and builds no request in any case.
    #[display("the provider built no request for any listed setup")]
    ResolverNeverRequests,
    /// A provider that declares `captures` and captures nothing in any case.
    #[display("the provider captured nothing for any listed fixture")]
    CapturerNeverMatches,
    /// A provider without `with_private_state()` that returns state.
    #[display("a provider without private state returned a state change")]
    StatelessState,
    /// A capture that returns a state change.
    #[display("a capture returned a state change")]
    CaptureStateChange,
    /// A fixture set of an expiring source without an expiry.
    #[display("the entry set for expiring source `{source}` carries no expiry")]
    MissingExpiry {
        /// The source.
        source: SourceDomain,
    },
    /// A stateful provider that accepts a bare-JSON-string stored state.
    #[display("the provider accepted a bare-string stored state as valid")]
    BareStringStateAccepted,
    /// A returned state that does not round-trip through the state type.
    #[display("a returned state does not round-trip through the state type")]
    StateRoundTrip,
    /// A stateful provider checked without a state type.
    #[display(
        "the provider keeps private state: use `check_conformance_with_state` with its state type"
    )]
    StatefulNeedsStateType,
    /// A provider bug that is not one of the checks above.
    #[display("the provider failed in `{stage}` with a configuration error")]
    ProviderFailed {
        /// The failing method.
        stage: &'static str,
    },
}

impl core::error::Error for ConformanceFailure {}

/// Checks that a returned state round-trips through the provider's state type.
type StateRoundTrip<'a, 'provider> = dyn Fn(&ProviderHarness<'provider>, &str) -> bool + 'a;

/// Drives one provider the way the coordinator does, so a vendor's tests
/// stay short.
///
/// A source with a [`with_stored`](Self::with_stored) record is a `Refresh`,
/// any other an `Initial`. Sources whose kind needs an input the harness
/// does not hold are left out, and with none left
/// [`request`](Self::request) returns `Ok(None)` without calling the
/// provider. So does a consent the provider's policy refuses.
pub struct ProviderHarness<'provider> {
    provider: &'provider dyn IdentityProvider,
    setup: HarnessSetup,
    random: SeededRandom,
    ec_id: Option<String>,
    last_request: Option<ProviderRequest>,
    recorded: Vec<Access>,
    state_decoded: bool,
}

impl<'provider> ProviderHarness<'provider> {
    /// A harness for `provider` with no inputs, the clock at 1,760,000,000
    /// and random seed 1.
    #[must_use]
    pub fn new(provider: &'provider dyn IdentityProvider) -> Self {
        let setup = HarnessSetup::default();
        Self {
            provider,
            random: setup.random(),
            setup,
            ec_id: None,
            last_request: None,
            recorded: Vec::new(),
            state_decoded: false,
        }
    }

    /// Replaces every input at once, for conformance cases.
    #[must_use]
    pub fn with_setup(mut self, setup: HarnessSetup) -> Self {
        self.random = setup.random();
        self.setup = setup;
        self
    }

    fn map_setup(mut self, change: impl FnOnce(HarnessSetup) -> HarnessSetup) -> Self {
        self.setup = change(core::mem::take(&mut self.setup));
        self
    }

    /// Sets the client IP.
    ///
    /// # Panics
    ///
    /// Panics when `ip` is not an IP address.
    #[must_use]
    pub fn with_client_ip(self, ip: &str) -> Self {
        self.map_setup(|setup| setup.with_client_ip(ip))
    }

    /// Sets the `User-Agent`.
    #[must_use]
    pub fn with_user_agent(self, user_agent: &str) -> Self {
        self.map_setup(|setup| setup.with_user_agent(user_agent))
    }

    /// Sets the page URL.
    ///
    /// # Panics
    ///
    /// Panics when `url` is not a URL.
    #[must_use]
    pub fn with_page_url(self, url: &str) -> Self {
        self.map_setup(|setup| setup.with_page_url(url))
    }

    /// Sets the region.
    ///
    /// # Panics
    ///
    /// Panics when `country` is not a country code.
    #[must_use]
    pub fn with_region(self, country: &str, subdivision: Option<&str>) -> Self {
        self.map_setup(|setup| setup.with_region(country, subdivision))
    }

    /// Sets the hashed email.
    #[must_use]
    pub fn with_hashed_email(self, email: HashedEmail) -> Self {
        self.map_setup(|setup| setup.with_hashed_email(email))
    }

    /// Sets the raw consent strings. Default: GDPR does not apply and no
    /// opt-outs.
    #[must_use]
    pub fn with_consent(self, consent: TestConsent) -> Self {
        self.map_setup(|setup| setup.with_consent(consent))
    }

    /// Gives the provider a stored record, valid or expired. Its source is
    /// the one its first entry names, or the provider's primary source.
    #[must_use]
    pub fn with_stored(self, stored: StoredId) -> Self {
        self.map_setup(|setup| setup.with_stored(stored))
    }

    /// Gives the provider its private state, as stored.
    ///
    /// # Panics
    ///
    /// Panics when `state` does not serialize.
    #[must_use]
    pub fn with_state(self, state: &impl Serialize) -> Self {
        self.map_setup(|setup| setup.with_state(state))
    }

    /// Sets the fixed clock.
    #[must_use]
    pub fn with_now(self, now: UnixTime) -> Self {
        self.map_setup(|setup| setup.with_now(now))
    }

    /// Reseeds the random source.
    #[must_use]
    pub fn with_random_seed(mut self, seed: u64) -> Self {
        self.setup = core::mem::take(&mut self.setup).with_random_seed(seed);
        self.random = self.setup.random();
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
        let (result, recorded, decoded) = if kinds.is_empty() || !self.consent_allows() {
            (Ok(None), Vec::new(), false)
        } else {
            self.call(kinds, |input| self.provider.request(input))
        };
        self.recorded.extend(recorded);
        self.state_decoded |= decoded;
        let request = result?;
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
        let (result, recorded, decoded) = self.call(self.kinds(), |input| {
            self.provider.response(&response, input)
        });
        self.recorded.extend(recorded);
        self.state_decoded |= decoded;
        result
    }

    /// Runs a capture as the coordinator does: the fixture's response is
    /// already decoded. A call no capture rule matches is `no_match`, as
    /// the coordinator never runs the provider for it.
    ///
    /// # Errors
    ///
    /// Returns what the provider returns.
    pub fn capture(
        &self,
        fixture: &CaptureFixture,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let descriptor = self.provider.descriptor();
        if !descriptor
            .captures()
            .is_some_and(|captures| captures.matches(&fixture.method, &fixture.path))
        {
            return Ok(Resolution::no_match());
        }
        let response =
            ProviderResponse::new(fixture.status, HeaderMap::new(), fixture.body.clone(), None);
        let stored = self.stored_by_source();
        let consent = ProviderConsent::from_context(&self.setup.consent.context());
        let input = CaptureInput::new(
            descriptor,
            &fixture.path,
            &response,
            &stored,
            &consent,
            "www.example.com",
            self.setup.now(),
        );
        self.provider.capture(&input)
    }

    /// Resolves an expiry or refresh against the fixed clock.
    #[must_use]
    pub fn resolve(&self, when: When) -> UnixTime {
        when.resolve(self.setup.now())
    }

    /// The replacement state in a result, decoded, for assertions.
    #[must_use]
    pub fn state_of<T: DeserializeOwned>(&self, resolution: &Resolution) -> Option<T> {
        match resolution.state_change() {
            StateChange::Replace(json) => serde_json::from_str(json).ok(),
            StateChange::Keep | StateChange::Clear => None,
        }
    }

    /// The EIDs bidders would see for a result: the section 4.5 rules, then
    /// the descriptor's provenance default for an entry that sets none of
    /// `inserter`, `matcher` and `mm` (section 7.1).
    #[must_use]
    pub fn output_eids(&self, resolution: &Resolution) -> Vec<Eid> {
        let sets = entry_sets(resolution.eids(), self.setup.now());
        let mut order: Vec<&SourceDomain> = Vec::new();
        for eid in resolution.eids() {
            if !order.contains(&eid.source()) {
                order.push(eid.source());
            }
        }
        let descriptor = self.provider.descriptor();
        let mut output = Vec::new();
        for source in order {
            let Some(set) = sets.sets.get(source) else {
                continue;
            };
            for entry in &set.entries {
                let mut eid = Eid {
                    source: source.as_str().to_owned(),
                    uids: entry
                        .uids
                        .iter()
                        .map(|uid| Uid {
                            id: uid.id.clone(),
                            atype: uid.atype,
                            ext: uid.ext.clone(),
                        })
                        .collect(),
                    inserter: entry.inserter.clone(),
                    matcher: entry.matcher.clone(),
                    mm: entry.mm,
                    ext: entry.ext.clone(),
                };
                if let Some(default) = descriptor.provenance(source)
                    && eid.inserter.is_none()
                    && eid.matcher.is_none()
                    && eid.mm.is_none()
                {
                    eid.inserter = default.inserter().map(str::to_owned);
                    eid.matcher = default.matcher().map(str::to_owned);
                    eid.mm = default.mm();
                }
                output.push(eid);
            }
        }
        output
    }

    /// Runs only the cases passed in, never the harness's own inputs, and
    /// fails the test on the first failed check. A provider declared
    /// `with_private_state()` fails here with a pointer to
    /// [`check_conformance_with_state`](Self::check_conformance_with_state).
    ///
    /// # Panics
    ///
    /// Panics when the provider does not conform.
    pub fn check_conformance(&self, cases: &ConformanceCases) {
        self.try_check_conformance(cases)
            .expect("should conform: the provider failed a conformance check");
    }

    /// As [`check_conformance`](Self::check_conformance), for a provider with
    /// private state of type `S`. Also checks that `S` round-trips through
    /// `replacing_state` and `input.state()`.
    ///
    /// # Panics
    ///
    /// Panics when the provider does not conform.
    pub fn check_conformance_with_state<S: Serialize + DeserializeOwned>(
        &self,
        cases: &ConformanceCases,
    ) {
        self.try_check_conformance_with_state::<S>(cases)
            .expect("should conform: the provider failed a conformance check");
    }

    /// The same checks as [`check_conformance`](Self::check_conformance),
    /// returning the first failure instead of failing the test. A panic
    /// aborts a `wasm32-wasip1` test run, so the harness's own negative
    /// tests use this.
    ///
    /// # Errors
    ///
    /// The first failed check.
    pub fn try_check_conformance(
        &self,
        cases: &ConformanceCases,
    ) -> Result<(), Report<ConformanceFailure>> {
        self.conform(cases, None)
    }

    /// As [`try_check_conformance`](Self::try_check_conformance), for a
    /// provider with private state of type `S`.
    ///
    /// # Errors
    ///
    /// The first failed check.
    pub fn try_check_conformance_with_state<S: Serialize + DeserializeOwned>(
        &self,
        cases: &ConformanceCases,
    ) -> Result<(), Report<ConformanceFailure>> {
        self.conform(
            cases,
            Some(&|harness, json| harness.state_round_trips::<S>(json)),
        )
    }

    fn conform(
        &self,
        cases: &ConformanceCases,
        round_trip: Option<&StateRoundTrip<'_, 'provider>>,
    ) -> Result<(), Report<ConformanceFailure>> {
        let descriptor = self.provider.descriptor();
        let stateful = descriptor
            .resolves()
            .is_some_and(Resolves::has_private_state);
        if stateful && round_trip.is_none() {
            return Err(Report::new(ConformanceFailure::StatefulNeedsStateType));
        }

        let mut requested = false;
        for case in &cases.resolve {
            let mut harness = Self::new(self.provider).with_setup(case.setup.clone());
            let request = harness.request();
            harness.check_recorded()?;
            let request = match request {
                Ok(Some(request)) => request,
                Ok(None) => continue,
                Err(error) => {
                    classify(error.current_context(), "request")?;
                    continue;
                }
            };
            requested = true;
            check_request(descriptor, &request)?;
            let headers: Vec<(&str, &str)> = case
                .headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            let resolution = harness.respond_raw(case.status, &headers, &case.body);
            harness.check_recorded()?;
            match resolution {
                Ok(resolution) => {
                    if !stateful && resolution.changes_state() {
                        return Err(Report::new(ConformanceFailure::StatelessState));
                    }
                    self.check_resolution(&resolution)?;
                    if let (StateChange::Replace(json), Some(round_trip)) =
                        (resolution.state_change(), round_trip)
                        && !round_trip(&harness, json)
                    {
                        return Err(Report::new(ConformanceFailure::StateRoundTrip));
                    }
                }
                Err(error) => classify(error.current_context(), "response")?,
            }
            if stateful {
                self.check_bare_string_state(case)?;
            }
        }
        if descriptor.resolves().is_some() && !requested {
            return Err(Report::new(ConformanceFailure::ResolverNeverRequests));
        }

        let mut captured = false;
        for fixture in &cases.capture {
            match self.capture(fixture) {
                Ok(resolution) => {
                    if resolution.changes_state() {
                        return Err(Report::new(ConformanceFailure::CaptureStateChange));
                    }
                    self.check_resolution(&resolution)?;
                    captured |= !resolution.is_no_match();
                }
                Err(error) => classify(error.current_context(), "capture")?,
            }
        }
        if descriptor.captures().is_some() && !captured {
            return Err(Report::new(ConformanceFailure::CapturerNeverMatches));
        }
        Ok(())
    }

    fn check_recorded(&self) -> Result<(), Report<ConformanceFailure>> {
        match self.recorded.first() {
            Some(access) => Err(Report::new(ConformanceFailure::UndeclaredInput {
                access: *access,
            })),
            None => Ok(()),
        }
    }

    fn check_resolution(&self, resolution: &Resolution) -> Result<(), Report<ConformanceFailure>> {
        let descriptor = self.provider.descriptor();
        for eid in resolution.eids() {
            if !descriptor.sources().contains(eid.source()) {
                return Err(Report::new(ConformanceFailure::UndeclaredSource {
                    source: eid.source().clone(),
                }));
            }
        }
        let sets = entry_sets(resolution.eids(), self.setup.now());
        if let Some(dropped) = sets.drops.first() {
            return Err(Report::new(ConformanceFailure::LimitHit {
                source: dropped.source.clone(),
                part: dropped.part.to_string(),
            }));
        }
        if let Some(source) = sets.oversized.first() {
            return Err(Report::new(ConformanceFailure::SetTooLarge {
                source: source.clone(),
            }));
        }
        for (source, set) in &sets.sets {
            if descriptor.is_expiring(source) && set.expires_at.is_none() {
                return Err(Report::new(ConformanceFailure::MissingExpiry {
                    source: source.clone(),
                }));
            }
        }
        Ok(())
    }

    /// Injects a stored state that is a bare JSON string, which no struct
    /// decodes, and fails when the provider decodes it as valid state.
    fn check_bare_string_state(
        &self,
        case: &ResolveCase,
    ) -> Result<(), Report<ConformanceFailure>> {
        let mut setup = case.setup.clone();
        setup.state_json = Some("\"bare\"".to_owned());
        let mut probe = Self::new(self.provider).with_setup(setup);
        if matches!(probe.request(), Ok(Some(_))) {
            let _ignored = probe.respond_raw(case.status, &[], &case.body);
        }
        if probe.state_decoded {
            return Err(Report::new(ConformanceFailure::BareStringStateAccepted));
        }
        Ok(())
    }

    /// Whether `json` decodes as `S`, serializes back to the same value and
    /// decodes again through `input.state()`.
    fn state_round_trips<S: Serialize + DeserializeOwned>(&self, json: &str) -> bool {
        let Ok(state) = serde_json::from_str::<S>(json) else {
            return false;
        };
        let Ok(again) = serde_json::to_string(&state) else {
            return false;
        };
        let same = serde_json::from_str::<serde_json::Value>(json).ok()
            == serde_json::from_str::<serde_json::Value>(&again).ok();
        let mut setup = self.setup.clone();
        setup.state_json = Some(again);
        let probe = Self::new(self.provider).with_setup(setup);
        #[allow(
            clippy::redundant_closure_for_method_calls,
            reason = "the method reference does not satisfy the higher-ranked closure bound"
        )]
        let (decoded, _, _) =
            probe.call(probe.kinds(), |input: &ResolveInput<'_>| input.state::<S>());
        same && matches!(decoded, Ok(Some(_)))
    }

    fn call<R>(
        &self,
        kinds: BTreeMap<SourceDomain, ResolveKind>,
        run: impl FnOnce(&ResolveInput<'_>) -> R,
    ) -> (R, Vec<Access>, bool) {
        let signals = self.signals();
        let input = ResolveInput::new(self.provider.descriptor(), kinds, &signals)
            .with_state_json(self.setup.state_json.clone());
        let result = run(&input);
        (result, input.recorded_errors(), input.state_was_decoded())
    }

    fn consent_allows(&self) -> bool {
        policy_allows(
            self.provider.descriptor().consent_policy(),
            &self.setup.consent.context(),
            PolicySignals {
                dnt: self.setup.consent.dnt,
            },
        )
    }

    fn stored_by_source(&self) -> BTreeMap<SourceDomain, StoredId> {
        self.setup
            .stored
            .iter()
            .map(|stored| {
                let source = stored
                    .eids()
                    .first()
                    .and_then(|eid| eid.source.parse().ok())
                    .unwrap_or_else(|| self.provider.descriptor().primary_source().clone());
                (source, stored.clone())
            })
            .collect()
    }

    fn available_inputs(&self) -> BTreeSet<Input> {
        let mut available = BTreeSet::new();
        for (input, present) in [
            (Input::ClientIp, self.setup.client_ip.is_some()),
            (Input::UserAgent, self.setup.user_agent.is_some()),
            (Input::PageUrl, self.setup.page_url.is_some()),
            (Input::Region, self.setup.region.is_some()),
            (Input::HashedEmail, self.setup.hashed_email.is_some()),
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
        let stored = self.stored_by_source();
        descriptor
            .sources()
            .iter()
            .filter_map(|source| {
                let kind = if stored.contains_key(source) {
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
            client_ip: self.setup.client_ip,
            user_agent: self.setup.user_agent.as_deref(),
            page_url: self.setup.page_url.clone(),
            region: self.setup.region.clone(),
            hashed_email: self.setup.hashed_email.clone(),
            consent: ProviderConsent::from_context(&self.setup.consent.context()),
            publisher_domain: "www.example.com",
            now: self.setup.now(),
            random: &self.random,
            ec_id: self.ec_id.clone(),
            stored: self.stored_by_source(),
        }
    }
}

/// Maps a provider error to the conformance check it breaks. Failures the
/// coordinator treats as a vendor outcome, such as a refusal or a body that
/// does not parse, are not bugs.
fn classify(
    error: &IdentityProviderError,
    stage: &'static str,
) -> Result<(), Report<ConformanceFailure>> {
    match error {
        IdentityProviderError::UndeclaredInput { access } => {
            Err(Report::new(ConformanceFailure::UndeclaredInput {
                access: *access,
            }))
        }
        error if error.is_not_implemented() => {
            Err(Report::new(ConformanceFailure::NotImplemented {
                method: stage,
            }))
        }
        IdentityProviderError::Config { .. } => {
            Err(Report::new(ConformanceFailure::ProviderFailed { stage }))
        }
        _ => Ok(()),
    }
}

/// The request rules of the coordinator, each as its own failure.
fn check_request(
    descriptor: &ProviderDescriptor,
    request: &ProviderRequest,
) -> Result<(), Report<ConformanceFailure>> {
    if request.url().scheme() != "https" {
        return Err(Report::new(ConformanceFailure::InsecureScheme));
    }
    let host = request.url().host_str().unwrap_or_default();
    let declared = descriptor.resolves().is_some_and(|resolves| {
        resolves
            .hosts()
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(host))
    });
    if !declared {
        return Err(Report::new(ConformanceFailure::UndeclaredHost));
    }
    if request.has_cookie_header() {
        return Err(Report::new(ConformanceFailure::CookieHeader));
    }
    if request
        .headers()
        .values()
        .any(|value| value.to_str().is_err())
    {
        return Err(Report::new(ConformanceFailure::InvalidHeader));
    }
    Ok(())
}

/// Builds the descriptor from a config with placeholder secret names and
/// from one with resolved secrets, and fails the test when the two differ
/// (spec 10.2).
///
/// # Panics
///
/// Panics when either config cannot describe a provider, or when the
/// descriptors differ.
pub fn check_descriptor_ignores_secrets<C: IdentityProviderConfig>(placeholder: &C, resolved: &C) {
    let from_placeholder = placeholder
        .build_descriptor()
        .expect("should build the descriptor from the placeholder config");
    let from_resolved = resolved
        .build_descriptor()
        .expect("should build the descriptor from the resolved config");
    assert_eq!(
        from_placeholder, from_resolved,
        "should build the same descriptor whatever the secrets"
    );
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
        /// The first-byte timeout the request's backend was registered with.
        timeout: Option<Duration>,
    }

    /// An HTTP client that answers per host, and records what it was sent.
    pub struct FakeHttpClient {
        routes: Mutex<BTreeMap<String, Route>>,
        delays: Mutex<BTreeMap<String, Duration>>,
        sent: Mutex<Vec<String>>,
        uris: Mutex<Vec<String>>,
        backend: Mutex<Option<Arc<FakeBackend>>>,
        earlier: Mutex<usize>,
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
                backend: Mutex::new(None),
                earlier: Mutex::new(0),
            }
        }

        /// Counts `count` backend requests as started before any provider
        /// call, as the origin or the auction would.
        pub fn started_earlier(&self, count: usize) {
            *self.earlier.lock().expect("should lock the earlier count") = count;
        }

        /// Makes a wait that outlasts the first-byte timeout `backend` last
        /// registered for the host fail once that timeout has passed, as a
        /// real backend does.
        pub fn honour_timeouts_of(&self, backend: &Arc<FakeBackend>) {
            *self.backend.lock().expect("should lock the backend") = Some(Arc::clone(backend));
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
        fn requests_started(&self) -> Option<usize> {
            let earlier = *self.earlier.lock().expect("should lock the earlier count");
            Some(earlier + self.sent.lock().expect("should lock the sent hosts").len())
        }

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
            let timeout = self
                .backend
                .lock()
                .expect("should lock the backend")
                .as_ref()
                .and_then(|backend| backend.last_first_byte_timeout(&host));
            Ok(PlatformPendingRequest::new(FakePending {
                host,
                route,
                delay,
                timeout,
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
            if let Some(timeout) = pending.timeout
                && pending.delay > timeout
            {
                std::thread::sleep(timeout);
                return Err(Report::new(PlatformError::HttpClient));
            }
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

        /// The first-byte timeout `host` was last registered with.
        fn last_first_byte_timeout(&self, host: &str) -> Option<Duration> {
            self.specs
                .lock()
                .expect("should lock the specs")
                .iter()
                .rev()
                .find(|spec| spec.host == host)
                .map(|spec| spec.first_byte_timeout)
        }

        /// The connect timeouts the backends were registered with, in order.
        pub fn connect_timeouts(&self) -> Vec<Option<Duration>> {
            self.specs
                .lock()
                .expect("should lock the specs")
                .iter()
                .map(|spec| spec.connect_timeout)
                .collect()
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
        fake_services_from(client, backend, ClientInfo::default())
    }

    /// [`fake_services`] for a request from `client_info`.
    #[cfg(test)]
    pub(crate) fn fake_services_from(
        client: &Arc<FakeHttpClient>,
        backend: &Arc<FakeBackend>,
        client_info: ClientInfo,
    ) -> RuntimeServices {
        client.honour_timeouts_of(backend);
        RuntimeServices::builder()
            .config_store(Arc::new(NoopConfigStore))
            .secret_store(Arc::new(NoopSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(backend.clone())
            .http_client(client.clone())
            .geo(Arc::new(NoopGeo))
            .client_info(client_info)
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
        ///
        /// It runs inside an executor, as the publisher handler does, so a
        /// nested `block_on` anywhere in the hold dispatch panics every hold
        /// test instead of only failing in production.
        pub(crate) fn run_hold(&mut self) -> HoldResults {
            let random = ThreadRandom;
            let deps = self.runtime.deps(
                &self.settings,
                &self.services,
                &random,
                Some(self.limiter.as_ref()),
            );
            futures::executor::block_on(run_hold(&deps, &mut self.context))
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

/// A proxied API call from a browser with a live row and capture-only
/// providers, for the tests of the capture removal write.
#[cfg(test)]
pub(crate) mod capture_fixture {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use http::{HeaderMap, Method, StatusCode};

    use super::fake_http::{FakeBackend, FakeHttpClient, fake_services};
    use super::{TEST_EC_ID, WriteCountingEcKv, allowing_consent};
    use crate::consent::ConsentContext;
    use crate::ec::EcContext;
    use crate::ec::identity::coordinator::{CaptureCall, run_capture};
    use crate::ec::identity::flush::retry_pending_removals;
    use crate::ec::identity::ids::{IdentityProviderId, SourceDomain};
    use crate::ec::identity::input::ThreadRandom;
    use crate::ec::identity::provider::IdentityProvider;
    use crate::ec::identity::request::WriterDecision;
    use crate::ec::identity::runtime::IdentityRuntime;
    use crate::ec::kv::KvIdentityGraph;
    use crate::ec::kv_types::KvEntry;
    use crate::platform::RuntimeServices;
    use crate::settings::{ProviderSettings, RevokeScope, Settings};
    use crate::test_support::tests::create_test_settings;

    /// The path every fixture capture call goes to.
    pub(crate) fn capture_path(provider: &str) -> String {
        format!("/integrations/{provider}/api/token")
    }

    /// Everything one capture call reads.
    pub(crate) struct CaptureFixture {
        pub(crate) settings: Settings,
        pub(crate) runtime: Arc<IdentityRuntime>,
        pub(crate) services: RuntimeServices,
        pub(crate) graph: KvIdentityGraph,
        pub(crate) store: Arc<WriteCountingEcKv>,
        pub(crate) context: EcContext,
    }

    impl CaptureFixture {
        /// A real browser's proxied call with `row` stored, read in phase 1,
        /// and every provider's `revoke_scope` at its default.
        pub(crate) fn new(
            providers: Vec<Arc<dyn IdentityProvider>>,
            row: Option<&KvEntry>,
        ) -> Self {
            Self::with_scopes(providers, row, &[])
        }

        /// As [`Self::new`], with `revoke_scope = "global"` for each provider
        /// listed in `global`.
        pub(crate) fn with_scopes(
            providers: Vec<Arc<dyn IdentityProvider>>,
            row: Option<&KvEntry>,
            global: &[&str],
        ) -> Self {
            let mut settings = create_test_settings();
            for id in global {
                settings.ec.identity.providers.insert(
                    (*id).to_owned(),
                    ProviderSettings {
                        revoke_scope: Some(RevokeScope::Global),
                        ..ProviderSettings::default()
                    },
                );
            }
            let runtime = Arc::new(
                IdentityRuntime::from_settings_with_providers(
                    &settings,
                    providers,
                    BTreeMap::<SourceDomain, IdentityProviderId>::new(),
                )
                .expect("should build the runtime"),
            );
            let client = Arc::new(FakeHttpClient::new());
            let backend = Arc::new(FakeBackend::new());
            let services = fake_services(&client, &backend);
            let (graph, store) = KvIdentityGraph::write_counting("capture-store");
            if let Some(row) = row {
                graph.create(TEST_EC_ID, row).expect("should seed the row");
            }
            let mut context = EcContext::new_for_test_with_ip(
                Some(TEST_EC_ID.to_owned()),
                allowing_consent(),
                Some("192.0.2.10".to_owned()),
            );
            context.set_identity_runtime(Arc::clone(&runtime));
            context.identity_mut().set_writer_decision(WriterDecision {
                writer: false,
                is_real_browser: true,
                user_agent: Some("ExampleBrowser/1.0".to_owned()),
                dnt: false,
                page_url: None,
            });
            context.set_kv_snapshot(graph.load_snapshot(TEST_EC_ID));
            store.reset_counts();
            Self {
                settings,
                runtime,
                services,
                graph,
                store,
                context,
            }
        }

        /// Replaces the request's consent.
        pub(crate) fn with_consent(mut self, consent: ConsentContext) -> Self {
            let mut context = EcContext::new_for_test_with_ip(
                Some(TEST_EC_ID.to_owned()),
                consent,
                Some("192.0.2.10".to_owned()),
            );
            context.set_identity_runtime(Arc::clone(&self.runtime));
            context.set_kv_snapshot(self.context.kv_snapshot().clone());
            context.identity_mut().set_real_browser(true);
            self.context = context;
            self
        }

        /// Makes the request a client that does not look like a browser.
        pub(crate) fn not_a_browser(mut self) -> Self {
            self.context.identity_mut().set_real_browser(false);
            self
        }

        /// Runs the capture for a 200 response holding `body` at `path`.
        pub(crate) fn capture(&mut self, path: &str, body: &serde_json::Value) {
            let bytes = serde_json::to_vec(body).expect("should serialize the body");
            self.capture_bytes(path, StatusCode::OK, &bytes);
        }

        /// Runs the capture for a response with `status` holding `bytes`.
        pub(crate) fn capture_bytes(&mut self, path: &str, status: StatusCode, bytes: &[u8]) {
            let random = ThreadRandom;
            let deps = self
                .runtime
                .deps(&self.settings, &self.services, &random, None);
            let headers = HeaderMap::new();
            run_capture(
                &deps,
                Some(&self.graph),
                &mut self.context,
                &CaptureCall {
                    method: &Method::POST,
                    path,
                    status,
                    headers: &headers,
                    body: bytes,
                },
            );
        }

        /// Runs the removal write's one retry after send.
        pub(crate) fn retry_after_send(&mut self) {
            retry_pending_removals(&mut self.context, &self.graph);
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
