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
use crate::test_support::ja4::{
    CHROME_USER_AGENT, CHROMIUM_JA4, FIREFOX_JA4, FIREFOX_USER_AGENT, PYTHON_JA4, SAFARI_JA4,
    SAFARI_USER_AGENT,
};

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
    /// A US state with opt-out privacy rules.
    UsCalifornia,
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
    /// The `User-Agent` header.
    pub user_agent: &'static str,
    /// The trusted JA4 the runner supplies, or `None` for no trusted JA4.
    pub ja4: Option<&'static str>,
    /// Fetch destination; a script is deliberately ineligible for creation.
    pub destination: &'static str,
}

impl Default for Navigation {
    fn default() -> Self {
        Self {
            consent: Consent::Granted,
            ec_cookie: None,
            geo: Geo::Gdpr,
            user_agent: CHROME_USER_AGENT,
            ja4: Some(CHROMIUM_JA4),
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
    /// A browser User-Agent with a non-browser trusted JA4 prevents creation.
    ContradictingJa4,
    /// A browser User-Agent with another browser engine's trusted JA4
    /// prevents creation.
    BrowserEngineMismatch,
    /// A current Firefox with its own trusted JA4 creates an identity.
    CurrentFirefox,
    /// A current Safari with its own trusted JA4 creates an identity.
    CurrentSafari,
}

impl Scenario {
    /// Navigation scenarios shared by all supported identity adapters.
    pub const NAVIGATION_SCENARIOS: [Self; 13] = [
        Self::Fresh,
        Self::WriteFailure,
        Self::UnknownGeo,
        Self::GeoFailure,
        Self::DeniedConsent,
        Self::MissingConsent,
        Self::Subresource,
        Self::Returning,
        Self::MissingTrustedJa4,
        Self::ContradictingJa4,
        Self::BrowserEngineMismatch,
        Self::CurrentFirefox,
        Self::CurrentSafari,
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
            Self::MissingTrustedJa4 => request.ja4 = None,
            Self::ContradictingJa4 => request.ja4 = Some(PYTHON_JA4),
            Self::BrowserEngineMismatch => {
                request.user_agent = FIREFOX_USER_AGENT;
                request.ja4 = Some(CHROMIUM_JA4);
            }
            Self::CurrentFirefox => {
                request.user_agent = FIREFOX_USER_AGENT;
                request.ja4 = Some(FIREFOX_JA4);
            }
            Self::CurrentSafari => {
                request.user_agent = SAFARI_USER_AGENT;
                request.ja4 = Some(SAFARI_JA4);
            }
            _ => {}
        }
        let response = adapter.navigate(&request);
        assert_document(&response);
        match self {
            Self::WriteFailure
            | Self::DeniedConsent
            | Self::MissingConsent
            | Self::Subresource
            | Self::ContradictingJa4
            | Self::BrowserEngineMismatch => {
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
                    // only request inputs, so its gate must refuse before any write; a
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
    /// Trusted location supplied by the adapter fixture.
    pub geo: Geo,
    /// Browser and partner headers.
    pub headers: Vec<(String, String)>,
    /// Serialized request body.
    pub body: String,
    /// Whether the application has configured its record service.
    pub store_enabled: bool,
    /// Whether bidding is enabled independently of identity storage.
    pub auction_enabled: bool,
    /// Publisher response status for fallback controls.
    pub origin_status: StatusCode,
    /// Publisher response type for fallback controls; empty omits the header.
    pub origin_content_type: &'static str,
    /// Whether trusted device signals classify the client as a known non-browser.
    pub known_non_browser: bool,
}

impl ApiRequest {
    /// Builds a local API request without navigation headers.
    #[must_use]
    pub fn new(method: http::Method, path: &str) -> Self {
        Self {
            method,
            path: path.to_owned(),
            geo: Geo::Gdpr,
            headers: Vec::new(),
            body: String::new(),
            store_enabled: true,
            auction_enabled: true,
            origin_status: StatusCode::OK,
            origin_content_type: "application/json",
            known_non_browser: false,
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
    /// Whether a runtime shared-cache directive survived finalization.
    pub edge_cache_headers_present: bool,
}

/// Applies identical partner and auction configuration to supported runners.
///
/// # Panics
/// Panics if the static partner or integration fixtures cannot deserialize.
pub fn configure_http_fixture(settings: &mut crate::settings::Settings) {
    // Shared JSON uses fixed TCF timestamps; age policy has separate consent tests.
    settings.consent.check_expiration = false;
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
        {"name":"Denied fixture","source_domain":"denied.example.com","bidstream_enabled":true,"mirror_enabled":true},
        {"name":"Unmirrored fixture","source_domain":"unmirrored.example.com","bidstream_enabled":true}
    ]))
    .expect("should configure conformance partner and SharedID");
}

// Static cross-language consent strings test transport and policy, not age.
/// The EC route's JSON, split into the mirror and its `sharedId` field.
fn route_json(observed: &ApiObservation) -> (serde_json::Value, Option<String>) {
    let mut json: serde_json::Value =
        serde_json::from_slice(&observed.response.body).expect("should decode EC route JSON");
    let sharedid = json
        .as_object_mut()
        .and_then(|object| object.remove("sharedId"))
        .map(|value| {
            value
                .as_str()
                .expect("should send sharedId as a string")
                .to_owned()
        });
    (json, sharedid)
}

/// The live `_pubcid` value the response sets, if any.
fn set_sharedid(response: &Observation) -> Option<&str> {
    response.cookies.iter().find_map(|cookie| {
        cookie
            .strip_prefix("_pubcid=")
            .and_then(|rest| rest.split(';').next())
            .filter(|_| !cookie.contains("Max-Age=0"))
    })
}

fn verify_ec_route_fixture(adapter: &mut impl Adapter, store: &RecordingStore) {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../trusted-server-js/lib/test/fixtures/ec-route-contract.json"
    ))
    .expect("should parse the shared browser/server contract fixture");
    for case in fixture["cases"]
        .as_array()
        .expect("should have contract cases")
    {
        let mut post = ApiRequest::new(http::Method::POST, "/_ts/api/v1/ec");
        post.body = case["body"].to_string();
        if case["geo"] == "us-california" {
            post.geo = Geo::UsCalifornia;
        }
        post.headers = vec![
            (
                "origin".to_owned(),
                "https://publisher.example.com".to_owned(),
            ),
            (
                "cookie".to_owned(),
                case["cookie"]
                    .as_str()
                    .expect("should have cookie input")
                    .to_owned(),
            ),
        ];
        let seeded = if case["seedIdentity"] == true {
            let mut seed = ApiRequest::new(http::Method::POST, "/_ts/api/v1/ec");
            seed.body = fixture["cases"][0]["body"].to_string();
            seed.headers.push((
                "origin".to_owned(),
                "https://publisher.example.com".to_owned(),
            ));
            let identity = assert_created(&adapter.exchange(&seed).response, store);
            post.headers[1].1.push_str(&format!("; ts-ec={identity}"));
            Some(identity)
        } else {
            None
        };
        let observed = adapter.exchange(&post);
        assert_eq!(
            observed.response.status.as_u16(),
            case["expected"]["status"]
                .as_u64()
                .expect("should have expected status") as u16,
            "should accept shared contract case: {}",
            case["name"]
        );
        assert_eq!(
            observed.origin_requests, 0,
            "should answer contract POST locally"
        );
        let (mirror, sharedid) = route_json(&observed);
        assert_eq!(
            mirror, case["expected"]["json"],
            "should return the expected mirror for {}",
            case["name"]
        );
        assert_eq!(
            sharedid.as_deref(),
            set_sharedid(&observed.response),
            "ID-9: should answer with the SharedID the response sets for {}",
            case["name"]
        );
        assert!(
            observed
                .cache_control
                .as_deref()
                .is_some_and(|value| value.contains("no-store") && value.contains("private")),
            "should keep consent responses private"
        );
        if case["expected"]["createsIdentity"] == true {
            assert_created(&observed.response, store);
        } else {
            assert!(
                !observed
                    .response
                    .cookies
                    .iter()
                    .any(|cookie| cookie.starts_with("ts-ec=") && !cookie.starts_with("ts-ec=;")),
                "should honor denied consent in {}",
                case["name"]
            );
            let identity = seeded.expect("should seed a withdrawal identity");
            assert_ec_expired(&observed.response);
            let row = block_on(
                KvIdentityGraph::new(store.clone()).get(&normalize_ec_id_for_kv(&identity)),
            )
            .expect("should read contract withdrawal")
            .expect("should retain withdrawal tombstone")
            .0;
            assert!(
                !row.consent.ok,
                "should durably withdraw in {}",
                case["name"]
            );
        }
        for key in block_on(store.list_keys_with_prefix("", 100))
            .expect("should list contract fixture keys")
        {
            block_on(store.delete(&key)).expect("should clean isolated contract record");
        }
    }
}

fn assert_ec_expired(response: &Observation) {
    assert!(
        response
            .cookies
            .iter()
            .any(|cookie| cookie.starts_with("ts-ec=;") && cookie.contains("Max-Age=0")),
        "should expire browser EC in the withdrawal response"
    );
}

/// Verifies that a client refused creation never writes on a navigation.
///
/// A known non-browser returning with a live EC and browser EIDs gets no
/// record write and no `SharedID`, while an allowed client with the same
/// request writes both. The EC route GET restores the stored `SharedID` only
/// for the allowed client. Consent withdrawal still tombstones for the refused
/// client.
///
/// # Panics
///
/// Panics when a refused client writes, the allowed control does not, or the
/// refused client's withdrawal does not tombstone.
pub fn verify_known_non_browser_navigation(adapter: &mut impl Adapter, store: &RecordingStore) {
    let graph = KvIdentityGraph::new(store.clone());
    let identity = format!("{}.nb0001", "c".repeat(64));
    let key = normalize_ec_id_for_kv(&identity);
    let mut live = crate::ec::kv_types::KvEntry::new(
        &crate::consent::types::ConsentContext::default(),
        None,
        100,
        "example.com",
    );
    live.consent.ok = true;
    block_on(graph.create(&key, &live)).expect("should seed a live returning identity");
    let eids = base64::engine::general_purpose::STANDARD.encode(
        serde_json::json!([{"source":"body.example.com","uids":[{"id":"cookie-uid","atype":1}]}])
            .to_string(),
    );
    let navigation = |known_non_browser: bool, consent: bool| {
        let mut request = ApiRequest::new(http::Method::GET, "/article");
        request.origin_content_type = "text/html";
        request.known_non_browser = known_non_browser;
        request.headers = vec![
            ("sec-fetch-dest".to_owned(), "document".to_owned()),
            ("sec-fetch-mode".to_owned(), "navigate".to_owned()),
            ("accept".to_owned(), "text/html".to_owned()),
            (
                "cookie".to_owned(),
                format!(
                    "{}; ts-ec={identity}; ts-eids={eids}",
                    tcf_consent_cookie(consent)
                ),
            ),
        ];
        request
    };
    let writes = |store: &RecordingStore| {
        store
            .events()
            .iter()
            .filter(|event| matches!(event, Event::Persisted(_) | Event::Rejected(_)))
            .count()
    };
    let issues_sharedid = |response: &Observation| {
        response
            .cookies
            .iter()
            .any(|cookie| cookie.starts_with("_pubcid=") && !cookie.contains("Max-Age=0"))
    };

    let before = block_on(graph.get(&key)).expect("should read the seeded identity");
    let writes_before = writes(store);
    let refused = adapter.exchange(&navigation(true, true));
    assert_document(&refused.response);
    assert_eq!(
        writes(store),
        writes_before,
        "should not write for a known non-browser navigation"
    );
    assert_eq!(
        block_on(graph.get(&key)).expect("should reread the identity"),
        before,
        "should leave a known non-browser's record unchanged"
    );
    assert!(
        !issues_sharedid(&refused.response),
        "should not issue SharedID to a known non-browser: {:?}",
        refused.response.cookies
    );

    // The same request from an allowed client proves the EIDs and SharedID
    // reach the write the refused client was denied.
    let allowed = adapter.exchange(&navigation(false, true));
    assert_document(&allowed.response);
    assert!(
        writes(store) > writes_before,
        "should write for the allowed control"
    );
    assert!(
        issues_sharedid(&allowed.response),
        "should issue SharedID to the allowed control: {:?}",
        allowed.response.cookies
    );

    // The EC route GET restores the stored SharedID only for an allowed client.
    for (known_non_browser, restored) in [(true, false), (false, true)] {
        let mut pixel = ApiRequest::new(http::Method::GET, "/_ts/api/v1/ec");
        pixel.known_non_browser = known_non_browser;
        pixel.headers.push((
            "cookie".to_owned(),
            format!("{}; ts-ec={identity}", tcf_consent_cookie(true)),
        ));
        let observed = adapter.exchange(&pixel);
        assert_eq!(
            observed.response.status,
            StatusCode::NO_CONTENT,
            "should answer the EC route GET"
        );
        assert_eq!(
            issues_sharedid(&observed.response),
            restored,
            "should restore SharedID on EC GET only for an allowed client: {:?}",
            observed.response.cookies
        );
    }

    let withdrawn = adapter.exchange(&navigation(true, false));
    assert_ec_expired(&withdrawn.response);
    let (row, _) = block_on(graph.get(&key))
        .expect("should read the withdrawal")
        .expect("should retain the tombstone");
    assert!(
        !row.consent.ok,
        "should tombstone a known non-browser's withdrawal"
    );
}

/// Verifies core-generated identity expiries and cache privacy on API responses.
///
/// # Panics
///
/// Panics if an adapter strips a required deletion or permits shared caching.
pub fn verify_api_identity_expiries(adapter: &mut impl Adapter) {
    for (consent, extra, expected) in [
        (
            false,
            "__Host-ts-eids-mirror=stale".to_owned(),
            vec!["__Host-ts-eids-mirror"],
        ),
        (
            true,
            "_pubcid=browser-id; _pubcid_cst=old; _pubcid_optout=1".to_owned(),
            vec!["_pubcid", "_pubcid_cst"],
        ),
        (
            false,
            format!(
                "ts-ec={}.abc123; __Host-ts-eids-mirror=stale; _pubcid=browser-id; _pubcid_cst=old",
                "a".repeat(64)
            ),
            vec!["ts-ec", "__Host-ts-eids-mirror", "_pubcid", "_pubcid_cst"],
        ),
    ] {
        let mut request = ApiRequest::new(http::Method::GET, "/publisher-api");
        request.headers.push((
            "cookie".to_owned(),
            format!("{}; {extra}", tcf_consent_cookie(consent)),
        ));
        let observed = adapter.exchange(&request);
        assert_eq!(
            observed.response.status,
            StatusCode::OK,
            "should finalize an ordinary API response"
        );
        for name in expected {
            assert!(
                observed.response.cookies.iter().any(|value| {
                    let cookie =
                        Cookie::parse(value.as_str()).expect("should parse generated expiry");
                    cookie.name() == name
                        && cookie.max_age().is_some_and(|age| age.whole_seconds() == 0)
                }),
                "should expire {name} on a non-document API response"
            );
        }
        assert!(
            observed.response.cookies.iter().all(|value| {
                let cookie = Cookie::parse(value.as_str()).expect("should parse response cookie");
                cookie.max_age().is_some_and(|age| age.whole_seconds() == 0)
            }),
            "should emit only expiry cookies on the API response"
        );
        assert!(
            !observed.edge_cache_headers_present,
            "should strip runtime shared-cache directives from expiry responses"
        );
        assert!(
            observed.cache_control.as_deref().is_some_and(|value| value
                .split(',')
                .map(str::trim)
                .any(|directive| directive == "private")
                && !value
                    .split(',')
                    .map(str::trim)
                    .any(|directive| directive == "public" || directive.starts_with("s-maxage"))),
            "should keep identity deletion responses private"
        );
    }
}

/// Executes the shared HTTP acceptance cases against a real adapter.
///
/// # Panics
/// Panics when the adapter violates identity persistence, locality or cookie scope.
pub fn verify_http_contract(adapter: &mut impl Adapter, store: &RecordingStore) {
    verify_ec_route_fixture(adapter, store);
    let mut orphan = ApiRequest::new(http::Method::POST, "/article");
    orphan.origin_content_type = "text/html";
    let orphan_id = format!("{}.abc123", "a".repeat(64));
    let eids = base64::engine::general_purpose::STANDARD.encode(serde_json::json!([{"source":"partner.example.com","uids":[{"id":"orphan-input","atype":1}]}]).to_string());
    orphan.headers = vec![
        ("sec-fetch-dest".to_owned(), "document".to_owned()),
        (
            "cookie".to_owned(),
            format!(
                "{}; ts-ec={orphan_id}; ts-eids={eids}",
                tcf_consent_cookie(true)
            ),
        ),
    ];
    let recovered = assert_created(&adapter.exchange(&orphan).response, store);
    assert_ne!(
        recovered, orphan_id,
        "should recover orphaned identity on POST"
    );
    for key in
        block_on(store.list_keys_with_prefix("", 100)).expect("should list orphan fixture keys")
    {
        block_on(store.delete(&key)).expect("should remove isolated orphan fixture");
    }
    for (method, status, mime) in [
        (http::Method::POST, StatusCode::OK, "text/html"),
        (http::Method::GET, StatusCode::NOT_FOUND, "text/html"),
        (
            http::Method::GET,
            StatusCode::SERVICE_UNAVAILABLE,
            "text/html",
        ),
        (http::Method::GET, StatusCode::NOT_MODIFIED, ""),
        (http::Method::GET, StatusCode::OK, "text/html"),
    ] {
        let mut document = ApiRequest::new(method.clone(), "/article");
        document.origin_status = status;
        document.origin_content_type = mime;
        document.headers = vec![
            ("sec-fetch-dest".to_owned(), "document".to_owned()),
            ("sec-fetch-mode".to_owned(), "navigate".to_owned()),
            ("accept".to_owned(), "text/html".to_owned()),
            ("cookie".to_owned(), tcf_consent_cookie(true)),
        ];
        let observed = adapter.exchange(&document);
        assert_eq!(
            observed.response.status, status,
            "should preserve publisher status"
        );
        assert_eq!(observed.origin_requests, 1, "should reach publisher");
        let identity = assert_created(&observed.response, store);
        document.headers.retain(|(name, _)| name != "cookie");
        document.headers.push((
            "cookie".to_owned(),
            format!("{}; ts-ec={identity}", tcf_consent_cookie(false)),
        ));
        // Withdrawal must also delete on a redirect without an HTML response.
        if method == http::Method::GET && status == StatusCode::OK {
            document.origin_status = StatusCode::FOUND;
            document.origin_content_type = "application/octet-stream";
        }
        let withdrawn = adapter.exchange(&document);
        assert_ec_expired(&withdrawn.response);
        let row =
            block_on(KvIdentityGraph::new(store.clone()).get(&normalize_ec_id_for_kv(&identity)))
                .expect("should read withdrawal")
                .expect("should retain tombstone")
                .0;
        assert!(
            !row.consent.ok,
            "should tombstone on every document method/status"
        );
        for key in block_on(store.list_keys_with_prefix("", 100))
            .expect("should list isolated document fixture keys")
        {
            block_on(store.delete(&key)).expect("should remove isolated document fixture key");
        }
    }
    for status in [
        StatusCode::MOVED_PERMANENTLY,
        StatusCode::FOUND,
        StatusCode::SEE_OTHER,
        StatusCode::TEMPORARY_REDIRECT,
        StatusCode::PERMANENT_REDIRECT,
        StatusCode::OK,
    ] {
        let mut excluded = ApiRequest::new(http::Method::GET, "/article");
        excluded.origin_status = status;
        excluded.origin_content_type = if status == StatusCode::OK {
            ""
        } else {
            "text/html"
        };
        excluded.headers = vec![
            ("sec-fetch-dest".to_owned(), "document".to_owned()),
            ("sec-fetch-mode".to_owned(), "navigate".to_owned()),
            ("cookie".to_owned(), tcf_consent_cookie(true)),
        ];
        assert_no_identity_cookies(&adapter.exchange(&excluded).response);
    }
    let ec_path = "/_ts/api/v1/ec";
    let tcf_cookie = tcf_consent_cookie(true);
    let tcf = tcf_cookie
        .strip_prefix("euconsent-v2=")
        .expect("should encode TCF");
    let consent_body = serde_json::json!({"tcString": tcf, "applicableSections": []}).to_string();
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
    assert_eq!(
        route_json(&created).1.as_deref(),
        set_sharedid(&created.response),
        "ID-9: should answer with the SharedID the EC POST sets"
    );
    let graph = KvIdentityGraph::new(store.clone());
    let key = normalize_ec_id_for_kv(&identity);
    let sharedid = assert_sharedid_cookies(&created.response);
    assert!(
        uuid::Uuid::parse_str(&sharedid).is_ok_and(|id| id.get_version_num() == 4),
        "should create a UUID v4 SharedID as Prebid writes: {sharedid}"
    );
    assert_eq!(
        block_on(graph.get(&key))
            .expect("should read created SharedID record")
            .expect("should retain created record")
            .0
            .ids["pubcid.org"]
            .uid,
        sharedid,
        "should store the created SharedID in the record it sets"
    );
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
    for companion in ["", "; _pubcid_cst=AAAAAA%3D%3D"] {
        let mut stale = ApiRequest::new(http::Method::POST, ec_path);
        stale.body = consent_body.clone();
        stale.headers = vec![
            (
                "origin".to_owned(),
                "https://publisher.example.com".to_owned(),
            ),
            (
                "cookie".to_owned(),
                format!("{tcf_cookie}; ts-ec={identity}; _pubcid={sharedid}{companion}"),
            ),
        ];
        let stale = adapter.exchange(&stale);
        assert_eq!(
            stale.response.status,
            StatusCode::OK,
            "should accept a stale companion"
        );
        let set = identity_cookie_names(&stale.response);
        assert_eq!(
            set,
            vec!["_pubcid_cst"],
            "should re-set only the companion for {companion:?}: {:?}",
            stale.response.cookies
        );
    }

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
        assert_eq!(
            response.response.cookies.len(),
            1,
            "should emit only the required mirror revocation expiry, filtering the retired marker"
        );
        let cookie = Cookie::parse(response.response.cookies[0].as_str())
            .expect("should parse mirror expiry");
        assert_eq!(
            cookie.name(),
            "__Host-ts-eids-mirror",
            "should revoke a stale mirror whose vendor is denied"
        );
        assert!(
            cookie.max_age().is_some_and(|age| age.whole_seconds() == 0),
            "should emit no positive identity cookie on publisher API responses"
        );
        assert!(
            !response.edge_cache_headers_present,
            "should strip shared cache directives from mirror revocation responses"
        );
        assert!(
            response
                .cache_control
                .as_deref()
                .is_some_and(|value| value.contains("private")),
            "should keep mirror revocation responses private"
        );
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
        (
            "unmirrored.example.com",
            "unmirrored-provider-id",
            Expiry::Never,
        ),
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
    assert_eq!(
        assert_sharedid_cookies(&restored.response),
        sharedid,
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

    for (geo, privacy, gpc, should_restore) in [
        (Geo::UsCalifornia, "1YNN", false, true),
        (Geo::UsCalifornia, "1YNN", true, false),
        (Geo::UsCalifornia, "1YYN", false, false),
        (Geo::Missing, "1YNN", false, false),
    ] {
        let before = block_on(graph.get(&key))
            .expect("should read the consented GET fixture")
            .expect("should retain the GET fixture record");
        let mut pixel = ApiRequest::new(http::Method::GET, ec_path);
        pixel.geo = geo;
        pixel.headers.push((
            "cookie".to_owned(),
            format!("ts-ec={identity}; us_privacy={privacy}"),
        ));
        if gpc {
            pixel.headers.push(("sec-gpc".to_owned(), "1".to_owned()));
        }
        let observed = adapter.exchange(&pixel);
        assert_eq!(
            observed.response.status,
            StatusCode::NO_CONTENT,
            "should answer EC GET under every current consent decision"
        );
        assert!(
            observed.response.body.is_empty(),
            "should keep the pixel response empty"
        );
        assert_eq!(observed.origin_requests, 0, "should answer EC GET locally");
        if should_restore {
            assert!(
                observed
                    .response
                    .cookies
                    .iter()
                    .any(|cookie| cookie.starts_with("_pubcid=") && !cookie.contains("Max-Age=0")),
                "should positively restore SharedID with current EC permission"
            );
        } else {
            assert_no_identity_cookies(&observed.response);
        }
        let after = block_on(graph.get(&key))
            .expect("should read the record after EC GET")
            .expect("should retain the live record after EC GET");
        assert_eq!(
            after.1, before.1,
            "should never write or tombstone a record from EC GET"
        );
        assert_eq!(
            serde_json::to_value(after.0).expect("should serialize the GET record"),
            serde_json::to_value(before.0).expect("should serialize the original record"),
            "should leave consent and IDs unchanged on EC GET"
        );
    }

    post.headers.push(("cookie".to_owned(), cookies.clone()));
    let mirrored = adapter.exchange(&post);
    let (mirror, _) = route_json(&mirrored);
    let cookie_mirror: serde_json::Value = mirrored
        .response
        .cookies
        .iter()
        .find_map(|cookie| cookie.strip_prefix("__Host-ts-eids-mirror="))
        .and_then(|cookie| cookie.split(';').next())
        .and_then(|value| URL_SAFE_NO_PAD.decode(value).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .expect("should set a decodable mirror cookie on the EC POST");
    assert_eq!(
        mirror, cookie_mirror,
        "should answer the EC POST with the mirror its cookie carries"
    );
    let mut eids = mirror["eids"].clone();
    let exp = eids[0]["uids"][0]
        .as_object_mut()
        .and_then(|uid| uid.remove("exp"))
        .and_then(|exp| exp.as_u64())
        .expect("should give a UID without stated expiry a numeric exp");
    let now = now_deciseconds() / 10;
    assert!(
        exp > now && exp <= now + 30 * 24 * 60 * 60,
        "should mirror a UID without stated expiry with an expiry within 30 days: exp={exp}, now={now}"
    );
    assert_eq!(
        eids,
        serde_json::json!([{"source":"mirror.example.com","uids":[{"id":"allowed-mirror-id","atype":1}]}]),
        "should expose only the live mirrored provider UID"
    );
    // ID-9 and WB-4: a returning reader whose mirror was set yesterday, so
    // its `exp` is one day short of today's cap, is unchanged: no identity
    // cookie is re-set and the route answers with the mirror it carries.
    let mut yesterday = cookie_mirror.clone();
    for eid in yesterday["eids"]
        .as_array_mut()
        .expect("should hold an EID array")
    {
        for uid in eid["uids"].as_array_mut().expect("should hold a UID array") {
            let exp = uid["exp"].as_u64().expect("should carry a numeric exp");
            uid["exp"] = serde_json::Value::from(exp - 24 * 60 * 60);
        }
    }
    let carried = URL_SAFE_NO_PAD.encode(yesterday.to_string());
    let mut returning = ApiRequest::new(http::Method::POST, ec_path);
    returning.body = consent_body.clone();
    returning.headers = vec![
        (
            "origin".to_owned(),
            "https://publisher.example.com".to_owned(),
        ),
        (
            "cookie".to_owned(),
            format!("{tcf_cookie}; {browser_cookies}; __Host-ts-eids-mirror={carried}"),
        ),
    ];
    let returning = adapter.exchange(&returning);
    assert_eq!(
        returning.response.status,
        StatusCode::OK,
        "should accept an unchanged returning reader"
    );
    assert_no_identity_cookies(&returning.response);
    let (kept_mirror, kept_sharedid) = route_json(&returning);
    assert_eq!(
        kept_mirror, yesterday,
        "should answer with the mirror the reader keeps"
    );
    assert_eq!(
        kept_sharedid.as_deref(),
        Some(sharedid.as_str()),
        "ID-9: should answer with the SharedID the reader keeps"
    );
    assert!(
        ![
            "body-fixture-id",
            "unmirrored-provider-id",
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
            serde_json::json!({"tcString":"invalid", "applicableSections":[]}).to_string(),
            StatusCode::BAD_REQUEST,
        ),
        (
            serde_json::json!({"tcString": format!("{tcf}.; ts-ec=injected")}).to_string(),
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
    failed_withdrawal.body = serde_json::json!({"tcString":denied_tcf.strip_prefix("euconsent-v2=").expect("should encode denied consent"),"applicableSections":[]}).to_string();
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
    assert_ec_expired(&withdrawn_api.response);
    assert!(
        !block_on(graph.get(&key))
            .expect("should read API withdrawal")
            .expect("should retain tombstone")
            .0
            .consent
            .ok,
        "should preserve authoritative API withdrawal with browser expiry"
    );

    let denied_tcf = tcf_consent_cookie(false);
    let denied_tcf = denied_tcf
        .strip_prefix("euconsent-v2=")
        .expect("should encode denied TCF");
    post.body = serde_json::json!({"tcString":denied_tcf,"applicableSections":[]}).to_string();
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
    verify_ec_route_sequence(adapter, store, tcf);
}

/// Removes every key from the store, a listing page at a time.
///
/// # Panics
///
/// Panics if a listed key cannot be deleted or survives its deletion, so a
/// store that does not shrink fails instead of looping.
fn clear_store(store: &RecordingStore) {
    loop {
        let keys =
            block_on(store.list_keys_with_prefix("", 100)).expect("should list fixture keys");
        if keys.is_empty() {
            return;
        }
        for key in keys {
            block_on(store.delete(&key)).expect("should remove isolated fixture key");
            assert!(
                !block_on(store.key_exists(&key)).expect("should check the removed key"),
                "should remove fixture key {key}"
            );
        }
    }
}

fn ec_post(body: String, cookie: Option<String>) -> ApiRequest {
    let mut post = ApiRequest::new(http::Method::POST, "/_ts/api/v1/ec");
    post.body = body;
    post.headers.push((
        "origin".to_owned(),
        "https://publisher.example.com".to_owned(),
    ));
    if let Some(cookie) = cookie {
        post.headers.push(("cookie".to_owned(), cookie));
    }
    post
}

/// Server side of the browser's ordered consent queue, plus the `SharedID`
/// opt-out on a first grant: grant A, withdrawal B, then grant C without EC.
fn verify_ec_route_sequence(adapter: &mut impl Adapter, store: &RecordingStore, granted_tcf: &str) {
    let grant = serde_json::json!({"tcString": granted_tcf, "applicableSections": []}).to_string();
    let denied_cookie = tcf_consent_cookie(false);
    let denied_tcf = denied_cookie
        .strip_prefix("euconsent-v2=")
        .expect("should encode denied TCF");
    let withdrawal =
        serde_json::json!({"tcString": denied_tcf, "applicableSections": []}).to_string();
    let graph = KvIdentityGraph::new(store.clone());

    clear_store(store);
    let opted_out = adapter.exchange(&ec_post(grant.clone(), Some("_pubcid_optout=1".to_owned())));
    let identity = assert_created(&opted_out.response, store);
    assert!(
        !opted_out.response.cookies.iter().any(|cookie| {
            (cookie.starts_with("_pubcid=") || cookie.starts_with("_pubcid_cst="))
                && !cookie.contains("Max-Age=0")
        }),
        "should set neither _pubcid nor _pubcid_cst for a reader with _pubcid_optout: {:?}",
        opted_out.response.cookies
    );
    assert_eq!(
        route_json(&opted_out).1,
        None,
        "SID-2: should answer a reader with _pubcid_optout without a SharedID"
    );
    assert!(
        !block_on(graph.get(&normalize_ec_id_for_kv(&identity)))
            .expect("should read opted-out record")
            .expect("should retain opted-out record")
            .0
            .ids
            .contains_key("pubcid.org"),
        "should store no SharedID for a reader with _pubcid_optout"
    );

    clear_store(store);
    let a = adapter.exchange(&ec_post(grant.clone(), None));
    let identity_a = assert_created(&a.response, store);
    let sharedid = assert_sharedid_cookies(&a.response);
    let b = adapter.exchange(&ec_post(
        withdrawal,
        Some(format!(
            "ts-ec={identity_a}; _pubcid={sharedid}; _pubcid_cst=AAAAAA%3D%3D; __Host-ts-eids-mirror=stale"
        )),
    ));
    assert_eq!(
        b.response.status,
        StatusCode::OK,
        "should accept withdrawal B"
    );
    for name in ["ts-ec", "_pubcid", "_pubcid_cst", "__Host-ts-eids-mirror"] {
        assert!(
            b.response
                .cookies
                .iter()
                .any(|cookie| cookie.starts_with(&format!("{name}=;"))
                    && cookie.contains("Max-Age=0")),
            "withdrawal B should expire {name}: {:?}",
            b.response.cookies
        );
    }
    assert!(
        !b.response
            .cookies
            .iter()
            .any(|cookie| !cookie.contains("Max-Age=0")),
        "withdrawal B should set no identity cookie: {:?}",
        b.response.cookies
    );
    let tombstoned = |store_graph: &KvIdentityGraph| {
        !block_on(store_graph.get(&normalize_ec_id_for_kv(&identity_a)))
            .expect("should read A's record")
            .expect("should retain A's tombstone")
            .0
            .consent
            .ok
    };
    assert!(
        tombstoned(&graph),
        "withdrawal B should tombstone A's record"
    );

    let c = adapter.exchange(&ec_post(grant, None));
    assert_eq!(c.response.status, StatusCode::OK, "should accept grant C");
    let identity_c = c
        .response
        .cookies
        .iter()
        .find_map(|cookie| cookie.strip_prefix("ts-ec="))
        .and_then(|cookie| cookie.split(';').next())
        .filter(|value| !value.is_empty())
        .expect("should mint a new EC for grant C as a first consent")
        .to_owned();
    assert_ne!(identity_c, identity_a, "grant C should not revive A");
    assert!(
        block_on(graph.get(&normalize_ec_id_for_kv(&identity_c)))
            .expect("should read C's record")
            .expect("should create a record of its own for grant C")
            .0
            .consent
            .ok,
        "grant C should create a live record"
    );
    assert!(tombstoned(&graph), "grant C should leave A tombstoned");
    clear_store(store);
}

/// Asserts the `SharedID` cookie and its companion carry Prebid's attributes
/// and returns the `SharedID` value.
fn assert_sharedid_cookies(response: &Observation) -> String {
    let attributes =
        "; Domain=.publisher.example.com; Path=/; Secure; SameSite=Lax; Max-Age=31536000";
    let value = |name: &str| {
        let (value, rest) = response
            .cookies
            .iter()
            .find_map(|cookie| cookie.strip_prefix(&format!("{name}=")))
            .and_then(|cookie| cookie.split_once(';'))
            .expect("should set the cookie with attributes");
        assert_eq!(
            format!(";{rest}"),
            attributes,
            "should set {name} readable by Prebid, Secure and Lax for one year"
        );
        assert!(!value.is_empty(), "should set a non-empty {name}");
        value.to_owned()
    };
    let sharedid = value("_pubcid");
    value("_pubcid_cst");
    sharedid
}

fn identity_cookie_names(response: &Observation) -> Vec<&str> {
    response
        .cookies
        .iter()
        .filter_map(|cookie| cookie.split_once('=').map(|(name, _)| name))
        .filter(|name| {
            [
                "ts-ec",
                "_pubcid",
                "_pubcid_cst",
                "__Host-ts-eids-mirror",
                "ts-ec-pull-complete",
            ]
            .contains(name)
        })
        .collect()
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
    post.body = serde_json::json!({"tcString":cookie.strip_prefix("euconsent-v2=").expect("should encode TCF"),"applicableSections":[]}).to_string();
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
