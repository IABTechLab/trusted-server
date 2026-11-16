//! Portable record-store fixtures for adapter identity conformance.

use std::sync::{Arc, Mutex};

use cookie::Cookie;
use error_stack::Report;

use crate::consent::now_deciseconds;
use crate::ec::normalize_ec_id_for_kv;
use http::StatusCode;

use crate::ec::generation::is_valid_ec_id;
use crate::ec::kv::KvIdentityGraph;
use crate::ec::kv_backend::test_support::InMemoryEcKv;
use crate::ec::kv_backend::{EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteOutcome};
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
    fail_writes: bool,
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
            fail_writes,
        }))
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

impl EcKvStore for RecordingStore {
    fn store_name(&self) -> &str {
        self.0.inner.store_name()
    }

    fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
        self.0.inner.lookup(key)
    }

    fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
        self.0.inner.key_exists(key)
    }

    fn insert(
        &self,
        key: &str,
        write: EcKvWrite<'_>,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
        if self.0.fail_writes {
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
        let outcome = self.0.inner.insert(key, write)?;
        if outcome == EcKvWriteOutcome::Written {
            self.0
                .events
                .lock()
                .expect("should lock fixture events")
                .push(Event::Persisted(key.to_owned()));
        }
        Ok(outcome)
    }

    fn list_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<String>, Report<TrustedServerError>> {
        self.0.inner.list_keys_with_prefix(prefix, limit)
    }

    fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
        self.0.inner.delete(key)
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
    /// Whether to supply the current browser policy's positive metadata.
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
    /// Missing browser metadata is refused on the inspected pre-T12 baseline.
    MissingBrowserMetadata,
}

impl Scenario {
    /// The complete current Fastly scenario set, with no runtime-only cases.
    pub const FASTLY_BASELINE: [Self; 9] = [
        Self::Fresh,
        Self::WriteFailure,
        Self::UnknownGeo,
        Self::GeoFailure,
        Self::DeniedConsent,
        Self::MissingConsent,
        Self::Subresource,
        Self::Returning,
        Self::MissingBrowserMetadata,
    ];

    /// Executes the scenario and checks the response against authoritative storage.
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
            store
                .list_keys_with_prefix("", 100)
                .expect("should inspect empty store")
                .is_empty(),
            "should start every scenario without seeded identities"
        );
        let mut request = Navigation::default();
        match self {
            Self::DeniedConsent => request.consent = Consent::Denied,
            Self::MissingConsent => request.consent = Consent::Missing,
            Self::Subresource => request.destination = "script",
            Self::MissingBrowserMetadata => request.browser = false,
            _ => {}
        }
        let response = adapter.navigate(&request);
        assert_document(&response);
        match self {
            Self::WriteFailure
            | Self::DeniedConsent
            | Self::MissingConsent
            | Self::Subresource
            | Self::MissingBrowserMetadata => {
                assert!(
                    ec_cookies(&response).is_empty(),
                    "should not issue or delete an EC for a fresh denied request"
                );
                assert!(
                    store
                        .list_keys_with_prefix("", 100)
                        .expect("should inspect fixture rows")
                        .is_empty(),
                    "should not persist an identity"
                );
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
                    let keys = store
                        .list_keys_with_prefix("", 100)
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
                    let (record, _) = graph
                        .get(&key)
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
                    let (record, _) = graph
                        .get(&key)
                        .expect("should inspect replayed identity")
                        .expect("should retain the existing record");
                    assert!(
                        record.consent.ok,
                        "should retain a live record after replay"
                    );
                    assert_eq!(
                        store
                            .list_keys_with_prefix("", 100)
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
        "should serve the publisher document"
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
    let (record, _) = graph
        .get(&key)
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
        .position(|event| *event == Event::ResponseReleased)
        .expect("should observe response release");
    assert!(
        persisted < released,
        "should persist before releasing the cookie"
    );
    assert_eq!(
        store
            .list_keys_with_prefix("", 100)
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
        KvIdentityGraph::new(store.clone())
            .create(identity, &record)
            .expect("should create the control record");
    }

    #[test]
    #[should_panic(expected = "should hold a live authoritative record")]
    fn identity_conformance_rejects_cookie_for_tombstone() {
        let store = RecordingStore::new(false);
        KvIdentityGraph::new(store.clone())
            .create(IDENTITY, &KvEntry::tombstone(100))
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
                self.0
                    .store
                    .delete(IDENTITY)
                    .expect("should replace the fixture record");
                KvIdentityGraph::new(self.0.store.clone())
                    .create(IDENTITY, &KvEntry::tombstone(100))
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
            let _error = KvIdentityGraph::new(self.0.clone())
                .create(
                    IDENTITY,
                    &KvEntry::new(&ConsentContext::default(), None, 100, "example.com"),
                )
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
}

#[cfg(test)]
mod portable_tests {
    use super::*;
    use crate::consent::types::ConsentContext;
    use crate::ec::kv::CreateIfAbsentOutcome;
    use crate::ec::kv_types::KvEntry;

    const IDENTITY: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.Ab1234";

    #[test]
    fn identity_conformance_preserves_conditional_write_preconditions() {
        let store = RecordingStore::new(false);
        let graph = KvIdentityGraph::new(store.clone());
        let record = KvEntry::new(&ConsentContext::default(), None, 100, "example.com");
        assert_eq!(
            graph
                .create_if_absent(IDENTITY, &record)
                .expect("should create absent identity"),
            CreateIfAbsentOutcome::Written,
            "should persist an absent key"
        );
        assert_eq!(
            graph
                .create_if_absent(IDENTITY, &record)
                .expect("should inspect existing identity"),
            CreateIfAbsentOutcome::AlreadyExists,
            "should refuse a duplicate create"
        );
    }
}
