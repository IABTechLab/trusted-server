//! Runtime services and stand-ins for the tests of every 51Degrees crate.
//!
//! [`RuntimeServices`] needs several things set before it will build, and
//! every 51Degrees module takes one. Written once here rather than in each
//! crate's tests, so a test that wants to prove a call was not made can do so
//! by counting rather than by inferring it from a failure that looks the same
//! as the outage case.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::key_value_store::{KvError, KvPage, KvStore};
use error_stack::Report;
use trusted_server_core::platform::{
    ClientInfo, DisabledGeo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore,
    PlatformError, PlatformGeoModule, PlatformHttpClient, PlatformHttpRequest,
    PlatformPendingRequest, PlatformResponse, PlatformSecretStore, PlatformSelectResult,
    PlatformValueCache, RuntimeServices, StoreId, StoreName, UnavailableKvStore,
};

/// Stores that answer nothing. No 51Degrees module reads either.
pub struct StubStore;

impl PlatformConfigStore for StubStore {
    fn get(&self, _store_name: &StoreName, _key: &str) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn put(
        &self,
        _store_id: &StoreId,
        _key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

impl PlatformSecretStore for StubStore {
    fn get_bytes(
        &self,
        _store_name: &StoreName,
        _key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn create(
        &self,
        _store_id: &StoreId,
        _name: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

/// A backend that resolves, so what a test proves is the call itself and not
/// the registration in front of it.
pub struct ResolvingBackend;

impl PlatformBackend for ResolvingBackend {
    fn predict_name(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Ok("fiftyone_degrees_test".to_owned())
    }

    fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Ok("fiftyone_degrees_test".to_owned())
    }
}

/// A key-value store held in memory for the life of one test.
///
/// Entries never expire, so what a test reads back does not depend on the
/// clock. The time to live each entry was written with is kept, so a test can
/// read it with [`MemoryKvStore::lifetime`].
#[derive(Debug, Default)]
pub struct MemoryKvStore {
    entries: Mutex<HashMap<String, Bytes>>,
    lifetimes: Mutex<HashMap<String, Duration>>,
}

impl MemoryKvStore {
    /// The time to live `key` was last written with, or `None` when it was
    /// written without one or not at all.
    ///
    /// # Panics
    ///
    /// When the lock is poisoned, which only a panicking test causes.
    #[must_use]
    pub fn lifetime(&self, key: &str) -> Option<Duration> {
        self.lifetimes
            .lock()
            .expect("should lock the test store")
            .get(key)
            .copied()
    }

    /// Every key written, sorted, so a test can say what was written and what
    /// was not.
    ///
    /// # Panics
    ///
    /// When the store's lock is poisoned, which only a panicking test causes.
    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .entries
            .lock()
            .expect("should lock the test store")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }
}

#[async_trait::async_trait(?Send)]
impl KvStore for MemoryKvStore {
    async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
        Ok(self
            .entries
            .lock()
            .expect("should lock the test store")
            .get(key)
            .cloned())
    }

    async fn put_bytes(&self, key: &str, value: Bytes) -> Result<(), KvError> {
        self.entries
            .lock()
            .expect("should lock the test store")
            .insert(key.to_owned(), value);
        self.lifetimes
            .lock()
            .expect("should lock the test store")
            .remove(key);
        Ok(())
    }

    async fn put_bytes_with_ttl(
        &self,
        key: &str,
        value: Bytes,
        ttl: Duration,
    ) -> Result<(), KvError> {
        self.put_bytes(key, value).await?;
        self.lifetimes
            .lock()
            .expect("should lock the test store")
            .insert(key.to_owned(), ttl);
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), KvError> {
        self.entries
            .lock()
            .expect("should lock the test store")
            .remove(key);
        self.lifetimes
            .lock()
            .expect("should lock the test store")
            .remove(key);
        Ok(())
    }

    async fn list_keys_page(
        &self,
        _prefix: &str,
        _cursor: Option<&str>,
        _limit: usize,
    ) -> Result<KvPage, KvError> {
        Err(KvError::Unavailable)
    }
}

/// A value cache held in memory, standing in for a host's shared cache.
///
/// Entries never expire. The lifetime each was put with is kept, so a test can
/// read it with [`MemoryValueCache::lifetime`].
#[derive(Debug, Default)]
pub struct MemoryValueCache {
    entries: Mutex<HashMap<String, (Bytes, Duration)>>,
}

impl MemoryValueCache {
    /// How many entries are held.
    ///
    /// # Panics
    ///
    /// When the lock is poisoned, which only a panicking test causes.
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .expect("should lock the test cache")
            .len()
    }

    /// Whether nothing is held.
    ///
    /// # Panics
    ///
    /// As [`MemoryValueCache::len`].
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The lifetime `key` was last put with, or `None` when it is not held.
    ///
    /// # Panics
    ///
    /// As [`MemoryValueCache::len`].
    #[must_use]
    pub fn lifetime(&self, key: &str) -> Option<Duration> {
        self.entries
            .lock()
            .expect("should lock the test cache")
            .get(key)
            .map(|(_, lifetime)| *lifetime)
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformValueCache for MemoryValueCache {
    async fn get(&self, key: &str) -> Option<Bytes> {
        self.entries
            .lock()
            .expect("should lock the test cache")
            .get(key)
            .map(|(value, _)| value.clone())
    }

    async fn put(&self, key: &str, value: Bytes, lifetime: Duration) {
        self.entries
            .lock()
            .expect("should lock the test cache")
            .insert(key.to_owned(), (value, lifetime));
    }
}

/// One request as it left this deployment.
#[derive(Debug, Clone)]
pub struct SentRequest {
    /// The method sent.
    pub method: String,
    /// The address asked for, which is where a credential in a query string
    /// would show up.
    pub uri: String,
    /// The body sent, which is where the form fields of a data request are.
    pub body: String,
    /// The headers sent, by name and value, which is where a credential sent
    /// in a header would show up.
    pub headers: Vec<(String, String)>,
}

/// One answer a [`ScriptedHttpClient`] gives, being a status and a body.
#[derive(Debug, Clone)]
pub struct ScriptedAnswer {
    status: u16,
    body: String,
}

impl ScriptedAnswer {
    /// `body` answered with `status`.
    #[must_use]
    pub fn new(status: u16, body: &str) -> Self {
        Self {
            status,
            body: body.to_owned(),
        }
    }
}

/// An HTTP client that answers a queue of answers in order and keeps what it
/// was asked. Once the queue runs out it answers `200` with `{}`.
#[derive(Debug)]
pub struct ScriptedHttpClient {
    answers: Mutex<VecDeque<ScriptedAnswer>>,
    sent: Mutex<Vec<SentRequest>>,
    delay: Option<Duration>,
}

impl ScriptedHttpClient {
    /// A client answering `bodies` in order, each with `200`.
    #[must_use]
    pub fn new(bodies: &[&str]) -> Self {
        Self::answering(bodies.iter().map(|body| ScriptedAnswer::new(200, body)))
    }

    /// A client giving `answers` in order, each with its own status.
    #[must_use]
    pub fn answering(answers: impl IntoIterator<Item = ScriptedAnswer>) -> Self {
        Self {
            answers: Mutex::new(answers.into_iter().collect()),
            sent: Mutex::new(Vec::new()),
            delay: None,
        }
    }

    /// Holds each answer back for `delay`, blocking the thread that sent the
    /// request, as a slow service would, so a test can have other requests
    /// arrive while one is waiting.
    #[must_use]
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    /// Everything this client was asked for, in order.
    ///
    /// # Panics
    ///
    /// When the log's lock is poisoned, which only a panicking test causes.
    pub fn sent(&self) -> Vec<SentRequest> {
        self.sent.lock().expect("should lock the sent log").clone()
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformHttpClient for ScriptedHttpClient {
    async fn send(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        let method = request.request.method().to_string();
        let uri = request.request.uri().to_string();
        let headers = request
            .request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        let body = request
            .request
            .into_body()
            .into_bytes_bounded(64 * 1024)
            .await
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        self.sent
            .lock()
            .expect("should lock the sent log")
            .push(SentRequest {
                method,
                uri,
                body,
                headers,
            });
        if let Some(delay) = self.delay {
            std::thread::sleep(delay);
        }
        let next = self
            .answers
            .lock()
            .expect("should lock the script")
            .pop_front()
            .unwrap_or_else(|| ScriptedAnswer::new(200, "{}"));
        let response = http::Response::builder()
            .status(next.status)
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(EdgeBody::from(next.body.into_bytes()))
            .expect("should build the test response");
        Ok(PlatformResponse::new(response))
    }

    async fn send_async(
        &self,
        _request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn select(
        &self,
        _pending: Vec<PlatformPendingRequest>,
    ) -> Result<PlatformSelectResult, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

/// The loading cache a test's services hold answers in: the 51Degrees
/// module's, which a deployment links, so what one call holds is what the
/// next one reads, with a memory copy of its own for each set of services, as
/// each instance has.
fn loading_caches() -> Arc<trusted_server_core::platform::LoadingCaches> {
    Arc::new(trusted_server_core::platform::LoadingCaches::every(
        Arc::new(trusted_server_loading_cache_51degrees::LoadingCaches::new()),
    ))
}

/// Services whose cloud calls are answered by `client`, sharing `store`.
pub fn services_with(
    client: Arc<ScriptedHttpClient>,
    store: Arc<MemoryKvStore>,
) -> RuntimeServices {
    RuntimeServices::builder()
        .permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
        .config_store(Arc::new(StubStore))
        .secret_store(Arc::new(StubStore))
        .kv_store(store)
        .backend(Arc::new(ResolvingBackend))
        .http_client(client)
        .geo(Arc::new(PlatformGeoModule::new(
            "none",
            Arc::new(DisabledGeo),
        )))
        .client_info(ClientInfo::default())
        .loading_caches(loading_caches())
        .prefixes(
            trusted_server_core::prefix::test_support::trusted_server_for("publisher.example"),
        )
        .build()
}

/// Services whose cloud calls are answered by `client`, with `cache` as the
/// host's value cache and no key-value store at all, as on a host whose store
/// is not configured.
pub fn services_with_value_cache(
    client: Arc<ScriptedHttpClient>,
    cache: Arc<MemoryValueCache>,
) -> RuntimeServices {
    RuntimeServices::builder()
        .permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
        .config_store(Arc::new(StubStore))
        .secret_store(Arc::new(StubStore))
        .kv_store(Arc::new(UnavailableKvStore))
        .value_cache(cache)
        .backend(Arc::new(ResolvingBackend))
        .http_client(client)
        .geo(Arc::new(PlatformGeoModule::new(
            "none",
            Arc::new(DisabledGeo),
        )))
        .client_info(ClientInfo::default())
        .loading_caches(loading_caches())
        .prefixes(
            trusted_server_core::prefix::test_support::trusted_server_for("publisher.example"),
        )
        .build()
}

/// The answer cache a test's cloud calls hold their answers in, with the
/// default lifetimes.
#[must_use]
pub fn answer_cache() -> crate::engine_cache::AnswerCache {
    crate::engine_cache::AnswerCache::new(
        &trusted_server_core::settings::LoadingCacheConfig::default(),
    )
}
