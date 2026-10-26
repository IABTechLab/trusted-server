//! Shared helpers for identity tests.

#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the test-utils library build compiles the crate-private helpers without their unit-test callers"
    )
)]

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::json;

use crate::consent::ConsentContext;
use crate::consent::jurisdiction::Jurisdiction;
use crate::consent::types::TcfConsent;
use crate::ec::identity::catalog::ProviderCatalog;
use crate::ec::identity::coordinator::{OutcomeContext, RequestFacts};
use crate::ec::identity::descriptor::{
    Captures, ConsentPolicy, Input, ProviderDescriptor, Resolves,
};
use crate::ec::identity::ids::{IdentityProviderId, SourceDomain, UnixTime};
use crate::ec::identity::input::SeededRandom;
use crate::ec::identity::policy::PolicySignals;
use crate::ec::identity::provider::IdentityProvider;
use crate::ec::identity::revision::{StateRevision, new_revision};
use crate::ec::kv_types::{KvEid, KvPartnerId, KvUid};
use crate::settings::{IdentitySettings, ProviderSettings};

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

struct DescriptorOnly(ProviderDescriptor);

impl IdentityProvider for DescriptorOnly {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.0
    }
}

/// A fake HTTP client, backend and provider for dispatch tests.
#[cfg(test)]
pub(crate) mod fake_http {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use error_stack::Report;

    use crate::ec::identity::descriptor::{ConsentPolicy, ProviderDescriptor, Resolves};
    use crate::ec::identity::exchange::{ProviderRequest, ProviderResponse};
    use crate::ec::identity::input::ResolveInput;
    use crate::ec::identity::provider::IdentityProvider;
    use crate::ec::identity::resolution::{IdentityProviderError, Resolution};
    use crate::platform::test_support::{NoopConfigStore, NoopGeo, NoopSecretStore};
    use crate::platform::{
        BackendNamingPolicy, ClientInfo, PlatformBackend, PlatformBackendSpec, PlatformError,
        PlatformHttpClient, PlatformHttpRequest, PlatformPendingRequest, PlatformResponse,
        PlatformSelectResult, RuntimeServices,
    };

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
    pub(crate) struct FakeHttpClient {
        routes: Mutex<BTreeMap<String, Route>>,
        delays: Mutex<BTreeMap<String, Duration>>,
        sent: Mutex<Vec<String>>,
    }

    impl FakeHttpClient {
        pub(crate) fn new() -> Self {
            Self {
                routes: Mutex::new(BTreeMap::new()),
                delays: Mutex::new(BTreeMap::new()),
                sent: Mutex::new(Vec::new()),
            }
        }

        /// Answers `host` with `status` and `body`.
        pub(crate) fn respond(&self, host: &str, status: u16, body: &[u8]) {
            self.respond_with_headers(host, status, body, &[]);
        }

        /// Answers `host` with `status`, `headers` and `body`.
        pub(crate) fn respond_with_headers(
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
        pub(crate) fn time_out(&self, host: &str) {
            self.routes
                .lock()
                .expect("should lock the routes")
                .insert(host.to_owned(), Route::Fail);
        }

        /// Makes the wait for `host` take `delay` of wall-clock time.
        pub(crate) fn delay(&self, host: &str, delay: Duration) {
            self.delays
                .lock()
                .expect("should lock the delays")
                .insert(host.to_owned(), delay);
        }

        /// The hosts requests were sent to, in order.
        pub(crate) fn sent_hosts(&self) -> Vec<String> {
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
    pub(crate) struct FakeBackend {
        specs: Mutex<Vec<PlatformBackendSpec>>,
    }

    impl FakeBackend {
        pub(crate) fn new() -> Self {
            Self {
                specs: Mutex::new(Vec::new()),
            }
        }

        /// The first-byte timeouts the backends were registered with, in order.
        pub(crate) fn first_byte_timeouts(&self) -> Vec<Duration> {
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
            }
        }

        /// Reads `user_agent()` without declaring it, and ignores the error.
        pub(crate) fn reading_undeclared_user_agent(mut self) -> Self {
            self.reads_user_agent = true;
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
            Ok(Some(ProviderRequest::get(input.endpoint()?)))
        }

        fn response(
            &self,
            response: &ProviderResponse,
            _input: &ResolveInput<'_>,
        ) -> Result<Resolution, Report<IdentityProviderError>> {
            response.success_text().map(|_| Resolution::no_match())
        }
    }
}
