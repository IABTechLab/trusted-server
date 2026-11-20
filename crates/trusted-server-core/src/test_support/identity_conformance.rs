//! Portable record-store fixtures for adapter identity conformance.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use cookie::Cookie;
use error_stack::Report;
use futures::executor::block_on;

use crate::consent::now_deciseconds;
use crate::ec::normalize_ec_id_for_kv;
use http::StatusCode;

use crate::ec::eid_types::{AgentType, EidFields, Expiry, ProviderId, UidValue, UnixSeconds};
use crate::ec::generation::is_valid_ec_id;
use crate::ec::kv::KvIdentityGraph;
use crate::ec::kv_backend::test_support::InMemoryEcKv;
use crate::ec::kv_backend::{EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome};
use crate::ec::kv_types::{KvEid, KvPartnerId, KvUid};
use crate::error::TrustedServerError;

/// An observed persistence or response-release boundary in a conformance run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A write successfully persisted the named key.
    Persisted(String),
    /// The fixture refused a write to the named key.
    Rejected(String),
    /// The runner began releasing a finalized response.
    ResponseReleased,
}

struct StoreState {
    inner: InMemoryEcKv,
    events: Mutex<Vec<Event>>,
    fail_writes: AtomicBool,
}

/// An inspectable store sharing authoritative state across request-local graphs.
///
/// Reads and conditional writes reuse the EC tests' generation-aware store.
/// Only available in test builds or with the `test-utils` feature.
#[derive(Clone)]
pub struct RecordingStore(Arc<StoreState>);

impl RecordingStore {
    /// Creates an empty store, optionally rejecting writes while preserving reads.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::test_support::identity_conformance::RecordingStore;
    /// let store = RecordingStore::new(false);
    /// assert!(store.events().is_empty());
    /// ```
    #[must_use]
    pub fn new(fail_writes: bool) -> Self {
        Self(Arc::new(StoreState {
            inner: InMemoryEcKv::new("identity-conformance"),
            events: Mutex::new(Vec::new()),
            fail_writes: AtomicBool::new(fail_writes),
        }))
    }

    /// Changes write failure without replacing the authoritative fixture rows.
    pub fn reject_writes(&self, rejected: bool) {
        self.0.fail_writes.store(rejected, Ordering::SeqCst);
    }

    /// Returns the ordered observations recorded so far.
    ///
    /// # Panics
    ///
    /// Panics if a previous fixture operation poisoned the event lock.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.0
            .events
            .lock()
            .expect("should lock fixture events")
            .clone()
    }

    /// Records the boundary at which the finalized response is released.
    ///
    /// # Panics
    ///
    /// Panics if a previous fixture operation poisoned the event lock.
    pub fn response_released(&self) {
        self.0
            .events
            .lock()
            .expect("should lock fixture events")
            .push(Event::ResponseReleased);
    }
}

#[async_trait::async_trait(?Send)]
impl EcKvStore for RecordingStore {
    fn store_name(&self) -> &str {
        self.0.inner.store_name()
    }

    async fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
        self.0.inner.lookup(key).await
    }

    async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
        self.0.inner.key_exists(key).await
    }

    async fn insert(
        &self,
        key: &str,
        write: EcKvWrite<'_>,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
        if self.0.fail_writes.load(Ordering::SeqCst) {
            self.0
                .events
                .lock()
                .expect("should lock fixture events")
                .push(Event::Rejected(key.to_owned()));
            return Err(Report::new(TrustedServerError::KvStore {
                store_name: self.store_name().to_owned(),
                message: "fixture rejects record creation".to_owned(),
            }));
        }
        let outcome = self.0.inner.insert(key, write).await?;
        if outcome == EcKvWriteOutcome::Written {
            self.0
                .events
                .lock()
                .expect("should lock fixture events")
                .push(Event::Persisted(key.to_owned()));
        }
        Ok(outcome)
    }

    async fn list_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<String>, Report<TrustedServerError>> {
        self.0.inner.list_keys_with_prefix(prefix, limit).await
    }

    async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
        self.0.inner.delete(key).await
    }
}

/// Consent sent through the adapter's real request parser in GDPR geo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// Serialized TCF Purpose 1 consent is granted.
    Granted,
    /// Serialized TCF Purpose 1 consent is denied.
    Denied,
    /// No consent cookie is sent.
    Missing,
}

/// Encodes current TCF v2 Purpose 1 and 4 consent for adapter fixtures.
///
/// # Examples
///
/// ```
/// use trusted_server_core::test_support::identity_conformance::tcf_consent_cookie;
/// assert!(tcf_consent_cookie(true).starts_with("euconsent-v2="));
/// ```
#[must_use]
pub fn tcf_consent_cookie(granted: bool) -> String {
    let mut core = [0_u8; 32];
    // TCF v2, followed by current Created and LastUpdated decisecond fields.
    core[0] = 2 << 2;
    let now = now_deciseconds();
    for offset in [6, 42] {
        for bit in 0..36 {
            if now & (1 << (35 - bit)) != 0 {
                core[(offset + bit) / 8] |= 1 << (7 - (offset + bit) % 8);
            }
        }
    }
    // Purpose 1 and Purpose 4 authorize storage and EID persistence.
    core[19] = if granted { 0x90 } else { 0 };
    format!("euconsent-v2={}", URL_SAFE_NO_PAD.encode(core))
}

/// Location available at the platform-service boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Geo {
    /// A GDPR location.
    Gdpr,
    /// No location was found.
    Missing,
    /// The location service failed.
    Failed,
}

/// A request to execute through an adapter's complete application lifecycle.
pub struct Navigation {
    /// Consent cookie representation to encode.
    pub consent: Consent,
    /// Previously issued EC cookie, if any.
    pub ec_cookie: Option<String>,
    /// The platform's location result.
    pub geo: Geo,
    /// Whether to supply trusted JA4 metadata where the adapter exposes it.
    pub browser: bool,
    /// Fetch destination; a script is deliberately ineligible for creation.
    pub destination: &'static str,
}

impl Default for Navigation {
    fn default() -> Self {
        Self {
            consent: Consent::Granted,
            ec_cookie: None,
            geo: Geo::Gdpr,
            browser: true,
            destination: "document",
        }
    }
}

/// Finalized response observed at delivery, after consuming the actual body.
pub struct Observation {
    /// Final HTTP status.
    pub status: StatusCode,
    /// Each raw Set-Cookie header, kept separately.
    pub cookies: Vec<String>,
    /// The completely consumed document body.
    pub body: Vec<u8>,
}

/// An adapter runner supplies platform fixtures, never EC business logic.
pub trait Adapter {
    /// Executes the request through routing, finalization and body delivery.
    fn navigate(&mut self, navigation: &Navigation) -> Observation;

    /// Executes a generic request through production routing and finalization.
    ///
    /// # Panics
    /// Panics for deliberately incomplete negative-control adapters.
    #[allow(
        clippy::panic,
        reason = "Negative-control adapters exercise only navigation; supported runners must implement this fixture hook"
    )]
    fn exchange(&mut self, _request: &ApiRequest) -> ApiObservation {
        panic!("should implement HTTP conformance for supported adapters");
    }
}

/// One reusable application conformance scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// A new cookie matches a new persisted row.
    Fresh,
    /// Rejected record creation emits no cookie.
    WriteFailure,
    /// A returning identity survives a missing location.
    UnknownGeo,
    /// A returning identity survives a failed location service.
    GeoFailure,
    /// Denied GDPR storage consent prevents creation.
    DeniedConsent,
    /// Missing GDPR storage consent prevents creation.
    MissingConsent,
    /// Subresource responses cannot create identity.
    Subresource,
    /// Returning navigation preserves identity.
    Returning,
    /// Missing trusted JA4 permits creation with consent and a trusted IP.
    MissingTrustedJa4,
}

impl Scenario {
    /// Navigation scenarios shared by all supported identity adapters.
    pub const NAVIGATION_SCENARIOS: [Self; 9] = [
        Self::Fresh,
        Self::WriteFailure,
        Self::UnknownGeo,
        Self::GeoFailure,
        Self::DeniedConsent,
        Self::MissingConsent,
        Self::Subresource,
        Self::Returning,
        Self::MissingTrustedJa4,
    ];

    /// Executes the scenario and checks the response against authoritative storage.
    ///
    /// The store reads block on their futures, so call this from a synchronous
    /// test, never from code already running under an executor.
    ///
    /// # Panics
    ///
    /// Panics on any missing document, cookie/record mismatch, failed fixture gate,
    /// identity rotation or persistence occurring after response release.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// Scenario::Fresh.verify(&mut adapter, &store);
    /// ```
    pub fn verify(self, adapter: &mut impl Adapter, store: &RecordingStore) {
        let graph = KvIdentityGraph::new(store.clone());
        assert!(
            block_on(store.list_keys_with_prefix("", 100))
                .expect("should inspect empty store")
                .is_empty(),
            "should start every scenario without seeded identities"
        );
        let mut request = Navigation::default();
        match self {
            Self::DeniedConsent => request.consent = Consent::Denied,
            Self::MissingConsent => request.consent = Consent::Missing,
            Self::Subresource => request.destination = "script",
            Self::MissingTrustedJa4 => request.browser = false,
            _ => {}
        }
        let response = adapter.navigate(&request);
        assert_document(&response);
        match self {
            Self::WriteFailure | Self::DeniedConsent | Self::MissingConsent | Self::Subresource => {
                assert!(
                    ec_cookies(&response).is_empty(),
                    "should not issue or delete an EC for a fresh denied request in {self:?}"
                );
                assert!(
                    block_on(store.list_keys_with_prefix("", 100))
                        .expect("should inspect fixture rows")
                        .is_empty(),
                    "should not persist an identity"
                );
                if self != Self::WriteFailure {
                    // These scenarios share Fresh's successful setup and change
                    // one input, so its gate must refuse before any write; a
                    // creation attempt that merely failed would not satisfy it.
                    assert!(
                        !store
                            .events()
                            .iter()
                            .any(|event| matches!(event, Event::Persisted(_) | Event::Rejected(_))),
                        "should refuse creation before attempting a write"
                    );
                }
                if self == Self::WriteFailure {
                    let events = store.events();
                    let rejected = events
                        .iter()
                        .position(
                            |event| matches!(event, Event::Rejected(key) if is_valid_ec_id(key)),
                        )
                        .expect("should reach the rejected creation write");
                    let released = events
                        .iter()
                        .position(|event| *event == Event::ResponseReleased)
                        .expect("should release the document response");
                    assert!(
                        rejected < released,
                        "should reject creation before releasing the response"
                    );
                }
            }
            _ => {
                let identity = assert_created(&response, store);
                if matches!(self, Self::UnknownGeo | Self::GeoFailure | Self::Returning) {
                    let key = normalize_ec_id_for_kv(&identity);
                    let keys = block_on(store.list_keys_with_prefix("", 100))
                        .expect("should inspect issued rows");
                    request.ec_cookie = Some(identity.clone());
                    request.geo = match self {
                        Self::UnknownGeo => Geo::Missing,
                        Self::GeoFailure => Geo::Failed,
                        _ => Geo::Gdpr,
                    };
                    let returning = adapter.navigate(&request);
                    assert_document(&returning);
                    let mut jar = IdentityJar(Some(identity.clone()));
                    jar.apply(&returning);
                    assert_eq!(
                        jar.0.as_deref(),
                        Some(identity.as_str()),
                        "should retain the reader's cookie"
                    );
                    let (record, _) = block_on(graph.get(&key))
                        .expect("should read returning record")
                        .expect("should retain the returning record");
                    assert!(
                        record.consent.ok,
                        "should retain a live record on returning navigation"
                    );
                    request.ec_cookie = jar.0;
                    let replay = adapter.navigate(&request);
                    assert_document(&replay);
                    let mut replay_jar = IdentityJar(request.ec_cookie.clone());
                    replay_jar.apply(&replay);
                    assert_eq!(
                        replay_jar.0.as_deref(),
                        Some(identity.as_str()),
                        "should preserve identity on replay"
                    );
                    let (record, _) = block_on(graph.get(&key))
                        .expect("should inspect replayed identity")
                        .expect("should retain the existing record");
                    assert!(
                        record.consent.ok,
                        "should retain a live record after replay"
                    );
                    assert_eq!(
                        block_on(store.list_keys_with_prefix("", 100))
                            .expect("should inspect returning rows"),
                        keys,
                        "should not create a second identity"
                    );
                }
            }
        }
    }
}

fn assert_document(response: &Observation) {
    assert_eq!(
        response.status,
        StatusCode::OK,
        "should serve the publisher document: {}",
        String::from_utf8_lossy(&response.body)
    );
    assert!(
        String::from_utf8_lossy(&response.body).contains("Example article"),
        "should consume the real publisher body"
    );
}

fn ec_cookies(response: &Observation) -> Vec<&str> {
    response
        .cookies
        .iter()
        .filter(|cookie| cookie.starts_with("ts-ec="))
        .map(String::as_str)
        .collect()
}

fn assert_created(response: &Observation, store: &RecordingStore) -> String {
    let cookies = ec_cookies(response);
    assert_eq!(cookies.len(), 1, "should issue exactly one EC cookie");
    let mut jar = IdentityJar(None);
    jar.apply(response);
    assert!(jar.0.is_some(), "should issue a live identity cookie");
    let identity = cookies[0]
        .strip_prefix("ts-ec=")
        .expect("should have EC prefix")
        .split(';')
        .next()
        .expect("should have cookie value");
    assert!(
        is_valid_ec_id(identity),
        "should issue a valid EC identifier"
    );
    let graph = KvIdentityGraph::new(store.clone());
    let key = normalize_ec_id_for_kv(identity);
    let (record, _) = block_on(graph.get(&key))
        .expect("should inspect issued identity")
        .expect("should persist the record matching the issued cookie");
    assert!(record.created > 0, "should persist creation metadata");
    assert!(record.consent.ok, "should hold a live authoritative record");
    let events = store.events();
    let persisted = events
        .iter()
        .position(|event| matches!(event, Event::Persisted(written) if written == &key))
        .expect("should observe the issued identity write");
    let released = events
        .iter()
        .rposition(|event| *event == Event::ResponseReleased)
        .expect("should observe response release");
    assert!(
        persisted < released,
        "should persist before releasing the cookie"
    );
    assert_eq!(
        block_on(store.list_keys_with_prefix("", 100))
            .expect("should inspect created records")
            .len(),
        1,
        "should create exactly one identity record"
    );
    identity.to_owned()
}

/// The reader's EC cookie state after applying each response's expiry directives.
struct IdentityJar(Option<String>);

impl IdentityJar {
    fn apply(&mut self, response: &Observation) {
        for raw in ec_cookies(response) {
            let cookie = Cookie::parse(raw).expect("should parse the response EC cookie");
            // Max-Age takes precedence over Expires, as in a browser cookie jar.
            let expired = cookie.max_age().map_or_else(
                || {
                    cookie.expires_datetime().is_some_and(|expiry| {
                        expiry.unix_timestamp() <= (now_deciseconds() / 10) as i64
                    })
                },
                |age| age.whole_seconds() <= 0,
            );
            self.0 = if expired {
                None
            } else {
                Some(cookie.value().to_owned())
            };
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::consent::types::ConsentContext;
    use crate::ec::kv_types::KvEntry;

    const IDENTITY: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.Ab1234";

    fn response(cookie: &str) -> Observation {
        Observation {
            status: StatusCode::OK,
            cookies: vec![cookie.to_owned()],
            body: b"Example article".to_vec(),
        }
    }

    fn create(store: &RecordingStore, identity: &str) {
        let mut record = KvEntry::new(&ConsentContext::default(), None, 100, "example.com");
        record.consent.ok = true;
        block_on(KvIdentityGraph::new(store.clone()).create(identity, &record))
            .expect("should create the control record");
    }

    #[test]
    #[should_panic(expected = "should hold a live authoritative record")]
    fn identity_conformance_rejects_cookie_for_tombstone() {
        let store = RecordingStore::new(false);
        block_on(KvIdentityGraph::new(store.clone()).create(IDENTITY, &KvEntry::tombstone(100)))
            .expect("should persist a withdrawal control");
        store.response_released();
        assert_created(&response(&format!("ts-ec={IDENTITY}")), &store);
    }

    #[test]
    #[should_panic(expected = "should persist the record matching the issued cookie")]
    fn identity_conformance_rejects_cookie_without_record() {
        let store = RecordingStore::new(false);
        store.response_released();
        assert_created(&response(&format!("ts-ec={IDENTITY}")), &store);
    }

    #[test]
    #[should_panic(expected = "should persist before releasing the cookie")]
    fn identity_conformance_rejects_unrelated_early_write() {
        let store = RecordingStore::new(false);
        create(&store, &IDENTITY.replace("Ab1234", "Cd5678"));
        store.response_released();
        create(&store, IDENTITY);
        assert_created(&response(&format!("ts-ec={IDENTITY}")), &store);
    }

    struct DeletingAdapter {
        store: RecordingStore,
        expiry: &'static str,
    }

    impl Adapter for DeletingAdapter {
        fn navigate(&mut self, navigation: &Navigation) -> Observation {
            let cookie = if navigation.ec_cookie.is_none() {
                create(&self.store, IDENTITY);
                format!("ts-ec={IDENTITY}")
            } else {
                assert_eq!(
                    navigation.ec_cookie.as_deref(),
                    Some(IDENTITY),
                    "should replay the issued cookie"
                );
                format!("ts-ec={IDENTITY}; {}", self.expiry)
            };
            self.store.response_released();
            response(&cookie)
        }
    }

    struct TombstoningAdapter(DeletingAdapter);

    impl Adapter for TombstoningAdapter {
        fn navigate(&mut self, navigation: &Navigation) -> Observation {
            let response = self.0.navigate(navigation);
            if navigation.ec_cookie.is_some() {
                block_on(self.0.store.delete(IDENTITY)).expect("should replace the fixture record");
                block_on(
                    KvIdentityGraph::new(self.0.store.clone())
                        .create(IDENTITY, &KvEntry::tombstone(100)),
                )
                .expect("should model a returning identity becoming unusable");
            }
            response
        }
    }

    #[test]
    #[should_panic(expected = "should retain a live record on returning navigation")]
    fn identity_conformance_rejects_tombstoned_returning_identity() {
        let store = RecordingStore::new(false);
        Scenario::Returning.verify(
            &mut TombstoningAdapter(DeletingAdapter {
                store: store.clone(),
                expiry: "Path=/",
            }),
            &store,
        );
    }

    #[test]
    #[should_panic(expected = "should retain the reader's cookie")]
    fn identity_conformance_rejects_negative_max_age() {
        let store = RecordingStore::new(false);
        Scenario::UnknownGeo.verify(
            &mut DeletingAdapter {
                store: store.clone(),
                expiry: "Max-Age=-1",
            },
            &store,
        );
    }

    #[test]
    #[should_panic(expected = "should retain the reader's cookie")]
    fn identity_conformance_rejects_past_expiry() {
        let store = RecordingStore::new(false);
        Scenario::UnknownGeo.verify(
            &mut DeletingAdapter {
                store: store.clone(),
                expiry: "Expires=Thu, 01 Jan 1970 00:00:00 GMT",
            },
            &store,
        );
    }

    struct LateRejectionAdapter(RecordingStore);

    impl Adapter for LateRejectionAdapter {
        fn navigate(&mut self, _navigation: &Navigation) -> Observation {
            self.0.response_released();
            let _error = block_on(KvIdentityGraph::new(self.0.clone()).create(
                IDENTITY,
                &KvEntry::new(&ConsentContext::default(), None, 100, "example.com"),
            ))
            .expect_err("should trigger the rejected write after release");
            Observation {
                status: StatusCode::OK,
                cookies: Vec::new(),
                body: b"Example article".to_vec(),
            }
        }
    }

    #[test]
    #[should_panic(expected = "should reject creation before releasing the response")]
    fn identity_conformance_rejects_late_failure() {
        let store = RecordingStore::new(true);
        Scenario::WriteFailure.verify(&mut LateRejectionAdapter(store.clone()), &store);
    }

    #[test]
    #[should_panic(expected = "should refuse creation before attempting a write")]
    fn identity_conformance_rejects_denied_consent_write_attempt() {
        let store = RecordingStore::new(true);
        Scenario::DeniedConsent.verify(&mut LateRejectionAdapter(store.clone()), &store);
    }
}

/// A request executed through an adapter's real routing and finalization.
pub struct ApiRequest {
    /// Request method.
    pub method: http::Method,
    /// First-party path.
    pub path: String,
    /// Browser and partner headers.
    pub headers: Vec<(String, String)>,
    /// Serialized request body.
    pub body: String,
    /// Whether the application has configured its record service.
    pub store_enabled: bool,
    /// Whether bidding is enabled independently of identity storage.
    pub auction_enabled: bool,
    /// Publisher response type for fallback controls.
    pub origin_content_type: &'static str,
}

impl ApiRequest {
    /// Builds a local API request without navigation headers.
    #[must_use]
    pub fn new(method: http::Method, path: &str) -> Self {
        Self {
            method,
            path: path.to_owned(),
            headers: Vec::new(),
            body: String::new(),
            store_enabled: true,
            auction_enabled: true,
            origin_content_type: "application/json",
        }
    }
}

/// Final response and the capturing publisher's observed request count.
pub struct ApiObservation {
    /// Fully consumed response.
    pub response: Observation,
    /// Outbound requests received by the publisher origin.
    pub origin_requests: usize,
    /// Response cache policy.
    pub cache_control: Option<String>,
}

/// Applies identical partner and auction configuration to supported runners.
///
/// # Panics
/// Panics if the static partner or integration fixtures cannot deserialize.
pub fn configure_http_fixture(settings: &mut crate::settings::Settings) {
    settings.integrations.insert_config("prebid", &serde_json::json!({
        "enabled": true, "external_bundle_url": "https://assets.example.com/prebid.js",
        "bundle": {"modules":{"bidder":["exampleBidderBidAdapter"]}},
        "managed_user_ids":[{"name":"sharedId","storage":{"type":"cookie","name":"_pubcid"}}]
    })).expect("should configure SharedID cookie storage");
    settings
        .proxy
        .allowed_domains
        .push("assets.example.com".to_owned());
    settings.ec.identity_providers.push(
        serde_json::from_value(serde_json::json!({
            "id":"denied-owner", "kind":"id5", "enabled":false, "tcf_vendor_id":42,
            "endpoint":"https://denied.example.com/id", "sources":["denied.example.com"]
        }))
        .expect("should configure a disabled provider whose vendor gates stored UIDs"),
    );
    settings.auction.enabled = true;
    settings.auction.providers.clear();
    settings.ec.partners = serde_json::from_value(serde_json::json!([
        {"name":"Fixture partner","source_domain":"partner.example.com","bidstream_enabled":true,
         "api_token":"example-partner-token-at-least-32-bytes"},
        {"name":"SharedID fixture","source_domain":"pubcid.org","bidstream_enabled":true},
        {"name":"Body fixture","source_domain":"body.example.com","bidstream_enabled":true},
        {"name":"Disabled body fixture","source_domain":"disabled-body.example.com","bidstream_enabled":true},
        {"name":"Mirror fixture","source_domain":"mirror.example.com","bidstream_enabled":true,"mirror_enabled":true},
        {"name":"Expired fixture","source_domain":"expired.example.com","bidstream_enabled":true,"mirror_enabled":true},
        {"name":"Denied fixture","source_domain":"denied.example.com","bidstream_enabled":true,"mirror_enabled":true}
    ]))
    .expect("should configure conformance partner and SharedID");
}

/// Executes the shared HTTP acceptance cases against a real adapter.
///
/// # Panics
/// Panics when the adapter violates identity persistence, locality or cookie scope.
pub fn verify_http_contract(adapter: &mut impl Adapter, store: &RecordingStore) {
    let mut posted_document = ApiRequest::new(http::Method::POST, "/article");
    posted_document.origin_content_type = "text/html";
    posted_document.headers = vec![
        ("sec-fetch-dest".to_owned(), "document".to_owned()),
        ("sec-fetch-mode".to_owned(), "navigate".to_owned()),
        ("accept".to_owned(), "text/html".to_owned()),
        ("cookie".to_owned(), tcf_consent_cookie(true)),
    ];
    let posted = adapter.exchange(&posted_document);
    assert_eq!(
        posted.response.status,
        StatusCode::OK,
        "should serve the POST document control"
    );
    assert_eq!(
        posted.origin_requests, 1,
        "should reach the publisher for POST document control"
    );
    assert_no_identity_cookies(&posted.response);
    assert!(
        block_on(store.list_keys_with_prefix("", 100))
            .expect("should inspect POST document rows")
            .is_empty(),
        "should never mint from a POST document"
    );
    let ec_path = "/_ts/api/v1/ec";
    let tcf_cookie = tcf_consent_cookie(true);
    let tcf = tcf_cookie
        .strip_prefix("euconsent-v2=")
        .expect("should encode TCF");
    let consent_body = serde_json::json!({"tcfString": tcf, "applicableSections": []}).to_string();
    let mut post = ApiRequest::new(http::Method::POST, ec_path);
    post.body = consent_body.clone();
    post.headers.push((
        "origin".to_owned(),
        "https://publisher.example.com".to_owned(),
    ));
    let created = adapter.exchange(&post);
    assert_eq!(
        created.response.status,
        StatusCode::OK,
        "should accept same-origin EC POST"
    );
    assert_eq!(created.origin_requests, 0, "should answer EC POST locally");
    assert!(
        created
            .cache_control
            .as_deref()
            .is_some_and(|policy| policy.contains("no-store")),
        "should never cache EC JSON"
    );
    let identity = assert_created(&created.response, store);
    assert!(
        created
            .response
            .cookies
            .iter()
            .any(|value| value.starts_with("_pubcid=")),
        "should issue the persisted SharedID on EC POST"
    );
    let graph = KvIdentityGraph::new(store.clone());
    let key = normalize_ec_id_for_kv(&identity);
    let cookies = format!("{tcf_cookie}; ts-ec={identity}");
    let browser_cookies = created
        .response
        .cookies
        .iter()
        .filter(|cookie| !cookie.contains("Max-Age=0"))
        .map(|cookie| cookie.split(';').next().expect("should read cookie pair"))
        .collect::<Vec<_>>()
        .join("; ");
    let mut unchanged = ApiRequest::new(http::Method::POST, ec_path);
    unchanged.body = consent_body.clone();
    unchanged.headers = vec![
        (
            "origin".to_owned(),
            "https://publisher.example.com".to_owned(),
        ),
        (
            "cookie".to_owned(),
            format!("{tcf_cookie}; {browser_cookies}"),
        ),
    ];
    let unchanged = adapter.exchange(&unchanged);
    assert_eq!(
        unchanged.response.status,
        StatusCode::OK,
        "should accept unchanged consent"
    );
    assert_no_identity_cookies(&unchanged.response);

    let mut batch = ApiRequest::new(http::Method::POST, "/_ts/api/v1/batch-sync");
    batch.headers.push((
        "authorization".to_owned(),
        "Bearer example-partner-token-at-least-32-bytes".to_owned(),
    ));
    batch.headers.push(("cookie".to_owned(), cookies.clone()));
    batch.body = serde_json::json!({"mappings":[{"ec_id":identity,"partner_uid":"batch-fixture-id","timestamp":100}]}).to_string();
    let synced = adapter.exchange(&batch);
    assert_eq!(
        synced.response.status,
        StatusCode::OK,
        "should process batch sync with the configured graph"
    );
    assert_eq!(synced.origin_requests, 0, "should never forward batch sync");
    assert_no_identity_cookies(&synced.response);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&synced.response.body)
            .expect("should decode batch JSON")["accepted"],
        1,
        "should acknowledge the real batch mapping"
    );
    assert_eq!(
        block_on(graph.get(&key))
            .expect("should read batch record")
            .expect("should retain EC")
            .0
            .ids["partner.example.com"]
            .uid,
        "batch-fixture-id",
        "should save batch UID into the EC row"
    );

    let mut identify = ApiRequest::new(http::Method::GET, "/_ts/api/v1/identify");
    identify.headers = batch.headers.clone();
    let identified = adapter.exchange(&identify);
    assert_eq!(
        identified.response.status,
        StatusCode::OK,
        "should identify from the same graph"
    );
    assert_eq!(
        identified.origin_requests, 0,
        "should never forward identify"
    );
    assert_no_identity_cookies(&identified.response);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&identified.response.body)
            .expect("should decode identify JSON")["uid"],
        "batch-fixture-id",
        "should return only the authenticated partner ID"
    );
    for mut request in [identify, batch] {
        request.store_enabled = false;
        let local = adapter.exchange(&request);
        assert_eq!(
            local.response.status,
            StatusCode::NOT_IMPLEMENTED,
            "should answer missing-store partner requests with 501"
        );
        assert_eq!(
            local.origin_requests, 0,
            "should never disclose partner requests to the publisher"
        );
        assert_no_identity_cookies(&local.response);
    }

    let mut auction = ApiRequest::new(http::Method::POST, "/auction");
    auction.headers.push(("cookie".to_owned(), cookies.clone()));
    auction.body = serde_json::json!({"adUnits":[{"code":"example-slot","mediaTypes":{"banner":{"sizes":[[300,250]]}}}], "eids":[{"source":"body.example.com","uids":[{"id":"body-fixture-id","atype":1}]}]}).to_string();
    let auctioned = adapter.exchange(&auction);
    assert_eq!(
        auctioned.response.status,
        StatusCode::OK,
        "should execute the configured no-bid auction"
    );
    assert_no_identity_cookies(&auctioned.response);
    let body_record = block_on(graph.get(&key))
        .expect("should read body record")
        .expect("should retain body record")
        .0;
    assert_eq!(
        body_record.ids["body.example.com"].uid, "body-fixture-id",
        "should persist auction body EIDs in the existing row"
    );
    assert_eq!(
        body_record.ids["partner.example.com"].uid, "batch-fixture-id",
        "should preserve the batch UID in the same row after body ingestion"
    );

    let mut disabled = ApiRequest::new(http::Method::POST, "/auction");
    disabled.auction_enabled = false;
    disabled
        .headers
        .push(("cookie".to_owned(), cookies.clone()));
    disabled.body = serde_json::json!({"adUnits":[{"code":"slot","mediaTypes":{"banner":{"sizes":[[300,250]]}}}],"eids":[{"source":"disabled-body.example.com","uids":[{"id":"disabled-body-id","atype":1}]}]}).to_string();
    let disabled = adapter.exchange(&disabled);
    assert!(
        disabled.response.status.is_success(),
        "should retain disabled no-bid response"
    );
    assert_no_identity_cookies(&disabled.response);
    assert_eq!(
        block_on(graph.get(&key))
            .expect("should read disabled body record")
            .expect("should retain live record")
            .0
            .ids["disabled-body.example.com"]
            .uid,
        "disabled-body-id",
        "should save body EIDs independently of bidding enablement"
    );

    for path in ["/asset.js", "/publisher-api"] {
        let mut asset = ApiRequest::new(http::Method::GET, path);
        asset.headers.push((
            "cookie".to_owned(),
            format!("{cookies}; __Host-ts-eids-mirror=stale; ts-ec-pull-complete=1"),
        ));
        asset
            .headers
            .push(("accept".to_owned(), "text/html".to_owned()));
        let response = adapter.exchange(&asset);
        assert_eq!(
            response.origin_requests,
            1,
            "should prove {path} reached the publisher: status={}, body={}",
            response.response.status,
            String::from_utf8_lossy(&response.response.body)
        );
        assert_no_identity_cookies(&response.response);
    }

    let (mut seeded, generation) = block_on(graph.get(&key))
        .expect("should read mirror fixture row")
        .expect("should retain mirror fixture row");
    for (source, uid, expiry) in [
        ("mirror.example.com", "allowed-mirror-id", Expiry::Never),
        (
            "expired.example.com",
            "expired-mirror-id",
            Expiry::At(UnixSeconds::new(1)),
        ),
        ("denied.example.com", "vendor-denied-id", Expiry::Never),
    ] {
        seeded.ids.insert(
            source.to_owned(),
            KvPartnerId::from_eids(
                vec![KvEid::new(
                    EidFields::default(),
                    vec![KvUid::new(
                        UidValue::try_from(uid).expect("should validate fixture UID"),
                        AgentType::try_from(1).expect("should validate fixture agent type"),
                        None,
                        Some(ProviderId::new("fixture-owner")),
                        expiry,
                    )],
                )],
                UnixSeconds::new(0),
            )
            .expect("should seed provider UID"),
        );
    }
    let seed_body = serde_json::to_string(&seeded).expect("should serialize provider fixture");
    assert_eq!(
        block_on(store.insert(
            &key,
            EcKvWrite {
                body: &seed_body,
                metadata: "{}",
                ttl: std::time::Duration::from_secs(86400),
                mode: EcKvWriteMode::IfGenerationMatch(generation)
            }
        ))
        .expect("should seed mirror record conditionally"),
        EcKvWriteOutcome::Written,
        "should preserve row generation while seeding mirror fixtures"
    );
    let mut get = ApiRequest::new(http::Method::GET, ec_path);
    get.headers.push(("cookie".to_owned(), cookies.clone()));
    let restored = adapter.exchange(&get);
    assert_eq!(
        restored.response.status,
        StatusCode::NO_CONTENT,
        "should return empty EC GET"
    );
    assert!(
        restored.response.body.is_empty(),
        "should keep pixel response empty"
    );
    assert!(
        restored
            .response
            .cookies
            .iter()
            .any(|value| value.starts_with("_pubcid=")),
        "should restore existing SharedID from its row"
    );
    assert!(
        !restored
            .response
            .cookies
            .iter()
            .any(|value| value.starts_with("ts-ec=")),
        "should not mint or refresh EC on GET"
    );
    let empty_get = adapter.exchange(&ApiRequest::new(http::Method::GET, ec_path));
    assert_eq!(
        empty_get.response.status,
        StatusCode::NO_CONTENT,
        "should support fresh pixel GET"
    );
    assert_no_identity_cookies(&empty_get.response);

    post.headers.push(("cookie".to_owned(), cookies.clone()));
    let mirrored = adapter.exchange(&post);
    let mirror: serde_json::Value =
        serde_json::from_slice(&mirrored.response.body).expect("should decode mirror-only EC JSON");
    assert_eq!(
        mirror["eids"],
        serde_json::json!([{"source":"mirror.example.com","uids":[{"id":"allowed-mirror-id","atype":1}]}]),
        "should expose only the live mirrored provider UID"
    );
    assert!(
        ![
            "body-fixture-id",
            "batch-fixture-id",
            "expired-mirror-id",
            "vendor-denied-id"
        ]
        .iter()
        .any(|uid| String::from_utf8_lossy(&mirrored.response.body).contains(uid)),
        "should never disclose a non-mirrored partner UID"
    );
    for origin in [Some("https://cross-site.example.com"), None] {
        let mut denied = ApiRequest::new(http::Method::POST, ec_path);
        denied.body = consent_body.clone();
        if let Some(origin) = origin {
            denied
                .headers
                .push(("origin".to_owned(), origin.to_owned()));
        }
        let denied = adapter.exchange(&denied);
        assert_eq!(
            denied.response.status,
            StatusCode::FORBIDDEN,
            "should reject untrusted EC origins"
        );
        assert_eq!(denied.origin_requests, 0, "should reject locally");
        assert_no_identity_cookies(&denied.response);
    }

    for (body, status) in [
        (
            serde_json::json!({"tcfString":"invalid", "applicableSections":[]}).to_string(),
            StatusCode::BAD_REQUEST,
        ),
        ("x".repeat(16 * 1024 + 1), StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let mut invalid = ApiRequest::new(http::Method::POST, ec_path);
        invalid.headers.push((
            "origin".to_owned(),
            "https://publisher.example.com".to_owned(),
        ));
        invalid.body = body;
        let invalid = adapter.exchange(&invalid);
        assert_eq!(
            invalid.response.status, status,
            "should bound and validate consent before identity work"
        );
        assert_eq!(
            invalid.origin_requests, 0,
            "should reject malformed consent locally"
        );
        assert_no_identity_cookies(&invalid.response);
    }
    let mut no_store = ApiRequest::new(http::Method::POST, ec_path);
    no_store.store_enabled = false;
    no_store.body = consent_body;
    no_store.headers.push((
        "origin".to_owned(),
        "https://publisher.example.com".to_owned(),
    ));
    let unavailable = adapter.exchange(&no_store);
    assert_eq!(
        unavailable.response.status,
        StatusCode::NOT_IMPLEMENTED,
        "should reject EC creation without a graph"
    );
    assert_no_identity_cookies(&unavailable.response);
    let denied_tcf = tcf_consent_cookie(false);
    let mut failed_withdrawal = ApiRequest::new(http::Method::POST, ec_path);
    failed_withdrawal.headers = post.headers.clone();
    failed_withdrawal.body = serde_json::json!({"tcfString":denied_tcf.strip_prefix("euconsent-v2=").expect("should encode denied consent"),"applicableSections":[]}).to_string();
    store.reject_writes(true);
    let failed_withdrawal = adapter.exchange(&failed_withdrawal);
    store.reject_writes(false);
    assert_eq!(
        failed_withdrawal.response.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "should not acknowledge a failed authoritative withdrawal"
    );
    assert!(
        block_on(graph.get(&key))
            .expect("should read failed withdrawal")
            .expect("should retain original live row")
            .0
            .consent
            .ok,
        "should prove the rejected write did not tombstone the record"
    );

    let mut denied_api = ApiRequest::new(http::Method::GET, "/publisher-api");
    denied_api.headers.push((
        "cookie".to_owned(),
        format!(
            "{}; ts-ec={identity}; __Host-ts-eids-mirror=stale; _pubcid=stale",
            tcf_consent_cookie(false)
        ),
    ));
    let withdrawn_api = adapter.exchange(&denied_api);
    assert_no_identity_cookies(&withdrawn_api.response);
    assert!(
        !block_on(graph.get(&key))
            .expect("should read API withdrawal")
            .expect("should retain tombstone")
            .0
            .consent
            .ok,
        "should preserve authoritative API withdrawal despite cookie suppression"
    );

    let denied_tcf = tcf_consent_cookie(false);
    let denied_tcf = denied_tcf
        .strip_prefix("euconsent-v2=")
        .expect("should encode denied TCF");
    post.body = serde_json::json!({"tcfString":denied_tcf,"applicableSections":[]}).to_string();
    let withdrawn = adapter.exchange(&post);
    assert!(
        withdrawn
            .response
            .cookies
            .iter()
            .any(|value| value.starts_with("ts-ec=;") && value.contains("Max-Age=0")),
        "should clear EC on an eligible withdrawal response"
    );
    assert!(
        !block_on(graph.get(&key))
            .expect("should read withdrawal")
            .expect("should retain tombstone")
            .0
            .consent
            .ok,
        "should never revive the withdrawn row"
    );
}

fn assert_no_identity_cookies(response: &Observation) {
    assert!(
        !response.cookies.iter().any(|cookie| [
            "ts-ec=",
            "_pubcid=",
            "_pubcid_cst=",
            "__Host-ts-eids-mirror=",
            "ts-ec-pull-complete="
        ]
        .iter()
        .any(|name| cookie.starts_with(name))),
        "should emit no identity cookie on this response: {:?}",
        response.cookies
    );
}

/// Verifies that a rejected EC POST write issues no unbacked identity.
///
/// # Panics
/// Panics if record failure produces identity cookies or reaches the publisher.
pub fn verify_failed_ec_creation(adapter: &mut impl Adapter, store: &RecordingStore) {
    let cookie = tcf_consent_cookie(true);
    let mut post = ApiRequest::new(http::Method::POST, "/_ts/api/v1/ec");
    post.headers.push((
        "origin".to_owned(),
        "https://publisher.example.com".to_owned(),
    ));
    post.body = serde_json::json!({"tcfString":cookie.strip_prefix("euconsent-v2=").expect("should encode TCF"),"applicableSections":[]}).to_string();
    let failed = adapter.exchange(&post);
    assert_eq!(
        failed.origin_requests, 0,
        "should answer failed EC creation locally"
    );
    assert!(
        !failed
            .response
            .cookies
            .iter()
            .any(|value| value.starts_with("ts-ec=") && !value.starts_with("ts-ec=;")),
        "should issue no live EC after failed create"
    );
    assert!(
        block_on(store.list_keys_with_prefix("", 100))
            .expect("should inspect failed EC create")
            .is_empty(),
        "should retain no failed creation"
    );
    assert!(
        store
            .events()
            .iter()
            .any(|event| matches!(event, Event::Rejected(_))),
        "should actually attempt the failing record write"
    );
    let events = store.events();
    let rejected = events
        .iter()
        .position(|event| matches!(event, Event::Rejected(_)))
        .expect("should observe rejected EC write");
    let released = events
        .iter()
        .rposition(|event| matches!(event, Event::ResponseReleased))
        .expect("should observe failed EC response release");
    assert!(
        rejected < released,
        "should observe EC write rejection before releasing its response"
    );
}

#[cfg(test)]
mod portable_tests {
    use std::time::Duration;

    use super::*;
    use crate::ec::kv_backend::EcKvWriteMode;

    const IDENTITY: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.Ab1234";

    /// A create-only write, the precondition identity creation relies on.
    const ADD: EcKvWrite<'static> = EcKvWrite {
        body: "{}",
        metadata: "{}",
        ttl: Duration::from_secs(60),
        mode: EcKvWriteMode::Add,
    };

    #[tokio::test(flavor = "current_thread")]
    async fn identity_conformance_preserves_conditional_write_preconditions() {
        let store = RecordingStore::new(false);
        assert_eq!(
            store
                .insert(IDENTITY, ADD)
                .await
                .expect("should create absent identity"),
            EcKvWriteOutcome::Written,
            "should persist an absent key"
        );
        assert_eq!(
            store
                .insert(IDENTITY, ADD)
                .await
                .expect("should inspect existing identity"),
            EcKvWriteOutcome::PreconditionFailed,
            "should refuse a duplicate create"
        );
    }
}
