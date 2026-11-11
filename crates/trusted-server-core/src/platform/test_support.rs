use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use base64::{Engine as _, engine::general_purpose};
use ed25519_dalek::SigningKey;
use error_stack::{Report, ResultExt as _};
use rand::rngs::OsRng;
use web_time::Instant;

use super::{
    ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec, PlatformCacheIntent,
    PlatformConfigStore, PlatformError, PlatformGeo, PlatformHttpClient, PlatformHttpRequest,
    PlatformPendingRequest, PlatformResponse, PlatformSecretStore, PlatformSelectResult,
    RuntimeServices, StoreId, StoreName,
};
use crate::request_signing::{JWKS_STORE_NAME, SIGNING_STORE_NAME};
use crate::test_support::scripted_client::{ScriptedHttpClient, TestClock};

pub(crate) struct NoopConfigStore;

#[async_trait::async_trait(?Send)]
impl PlatformConfigStore for NoopConfigStore {
    fn get(&self, _store_name: &StoreName, _key: &str) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn put(
        &self,
        _store_id: &StoreId,
        _key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

pub(crate) struct NoopSecretStore;

#[async_trait::async_trait(?Send)]
impl PlatformSecretStore for NoopSecretStore {
    fn get_bytes(
        &self,
        _store_name: &StoreName,
        _key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn create(
        &self,
        _store_id: &StoreId,
        _name: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

pub(crate) struct HashMapConfigStore {
    data: HashMap<String, String>,
}

impl HashMapConfigStore {
    pub(crate) fn new(data: HashMap<String, String>) -> Self {
        Self { data }
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformConfigStore for HashMapConfigStore {
    fn get(&self, _store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        self.data
            .get(key)
            .cloned()
            .ok_or_else(|| Report::new(PlatformError::ConfigStore))
    }

    async fn put(
        &self,
        _store_id: &StoreId,
        _key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

pub(crate) struct HashMapSecretStore {
    data: HashMap<String, Vec<u8>>,
}

impl HashMapSecretStore {
    pub(crate) fn new(data: HashMap<String, Vec<u8>>) -> Self {
        Self { data }
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformSecretStore for HashMapSecretStore {
    fn get_bytes(
        &self,
        _store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        self.data
            .get(key)
            .cloned()
            .ok_or_else(|| Report::new(PlatformError::SecretStore))
    }

    async fn create(
        &self,
        _store_id: &StoreId,
        _name: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

pub(crate) struct NoopBackend;

impl PlatformBackend for NoopBackend {
    fn naming_policy(&self) -> super::BackendNamingPolicy {
        super::BackendNamingPolicy::Axum
    }

    fn predict_name(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

pub(crate) struct NoopHttpClient;

// ?Send matches PlatformHttpClient. Body wraps LocalBoxStream which is !Send
// by design; see http.rs for the full rationale.
#[async_trait::async_trait(?Send)]
impl PlatformHttpClient for NoopHttpClient {
    async fn send(
        &self,
        _request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn send_async(
        &self,
        _request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    async fn select(
        &self,
        _pending_requests: Vec<PlatformPendingRequest>,
    ) -> Result<PlatformSelectResult, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

// ---------------------------------------------------------------------------
// StubBackend
// ---------------------------------------------------------------------------

/// Test stub for [`PlatformBackend`] that returns `"stub-backend"` for any
/// spec, allowing callers to proceed past backend registration.
pub(crate) struct StubBackend;

impl PlatformBackend for StubBackend {
    fn naming_policy(&self) -> super::BackendNamingPolicy {
        super::BackendNamingPolicy::Axum
    }

    fn predict_name(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Ok("stub-backend".to_owned())
    }

    fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Ok("stub-backend".to_owned())
    }
}

// ---------------------------------------------------------------------------
// StubHttpClient
// ---------------------------------------------------------------------------

/// Canned response carried by a [`PlatformPendingRequest`] through `send_async`
/// and resolved by [`StubHttpClient::select`].
struct StubPendingResponse {
    backend_name: String,
    cache_intent: PlatformCacheIntent,
    status: u16,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
}

/// Test stub for [`PlatformHttpClient`] that records call backend names and
/// returns pre-queued canned responses for `send`, `send_async`, and `select`.
///
/// Responses are stored as status/body/header parts to remain [`Send`].
/// [`PlatformResponse`] contains [`edgezero_core::body::Body`] which wraps a
/// `LocalBoxStream` that is `!Send`, so it cannot be stored directly in a
/// `Mutex` field.
///
/// Use [`push_response`](Self::push_response) to enqueue responses before
/// exercising the code under test, then inspect
/// [`recorded_backend_names`](Self::recorded_backend_names) to assert call
/// sites.
pub(crate) struct StubHttpClient {
    calls: Mutex<Vec<String>>,
    responses: Mutex<VecDeque<StubHttpResponse>>,
}

struct StubHttpResponse {
    status: u16,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
}

impl StubHttpClient {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            responses: Mutex::new(VecDeque::new()),
        }
    }

    /// Queue a canned response by status code and body bytes.
    pub fn push_response(&self, status: u16, body: Vec<u8>) {
        self.push_response_with_headers(status, body, Vec::<(String, String)>::new());
    }

    /// Queue a canned response with headers.
    pub fn push_response_with_headers(
        &self,
        status: u16,
        body: Vec<u8>,
        headers: Vec<(impl Into<String>, impl Into<String>)>,
    ) {
        let headers = headers
            .into_iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect();
        self.responses
            .lock()
            .expect("should lock responses")
            .push_back(StubHttpResponse {
                status,
                body,
                headers,
            });
    }

    /// Return backend names recorded across all `send` calls, in order.
    pub fn recorded_backend_names(&self) -> Vec<String> {
        self.calls.lock().expect("should lock calls").clone()
    }
}

// ?Send matches PlatformHttpClient. See http.rs for the full rationale.
#[async_trait::async_trait(?Send)]
impl PlatformHttpClient for StubHttpClient {
    async fn send(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        let request_is_head = request.request.method() == edgezero_core::http::Method::HEAD;
        self.calls
            .lock()
            .expect("should lock calls")
            .push(request.backend_name.clone());

        let response = self
            .responses
            .lock()
            .expect("should lock responses")
            .pop_front()
            .ok_or_else(|| Report::new(PlatformError::HttpClient))?;

        let edge_response = build_stub_pending_response(
            StubPendingResponse {
                backend_name: request.backend_name,
                cache_intent: request.cache_intent,
                status: response.status,
                body: response.body,
                headers: response.headers,
            },
            request_is_head,
        )?;

        Ok(PlatformResponse::new(edge_response))
    }

    async fn send_async(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
        if request.stream_response {
            return Err(Report::new(PlatformError::HttpClient)
                .attach("streaming responses are not supported with StubHttpClient send_async"));
        }

        let backend_name = request.backend_name.clone();
        let request_method = request.request.method().clone();
        self.calls
            .lock()
            .expect("should lock calls")
            .push(backend_name.clone());
        let response = self
            .responses
            .lock()
            .expect("should lock responses")
            .pop_front()
            .ok_or_else(|| Report::new(PlatformError::HttpClient))?;

        let pending = StubPendingResponse {
            backend_name: backend_name.clone(),
            cache_intent: request.cache_intent,
            status: response.status,
            body: response.body,
            headers: response.headers,
        };
        Ok(PlatformPendingRequest::new(pending)
            .with_response_handling(false, request_method)
            .with_backend_name(backend_name))
    }

    async fn select_until(
        &self,
        pending_requests: Vec<PlatformPendingRequest>,
        _deadline: Instant,
    ) -> Result<Option<PlatformSelectResult>, Report<PlatformError>> {
        // Match Fastly's per-provider rejection before waiting on network work.
        let mut unchecked = pending_requests.into_iter();
        let mut checked = Vec::with_capacity(unchecked.len());
        while let Some(pending) = unchecked.next() {
            match validate_deadline_cache_bypass(pending) {
                Ok(pending) => checked.push(pending),
                Err((backend_name, error)) => {
                    checked.extend(unchecked);
                    return Ok(Some(PlatformSelectResult {
                        ready: Err(error),
                        remaining: checked,
                        failed_backend_name: backend_name,
                    }));
                }
            }
        }
        self.select(checked).await.map(Some)
    }

    async fn wait_until(
        &self,
        pending: PlatformPendingRequest,
        _deadline: Instant,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        let pending = validate_deadline_cache_bypass(pending).map_err(|(_, error)| error)?;
        self.wait(pending).await
    }

    /// Always marks the first pending request in the input as ready (FIFO order).
    ///
    /// This differs from Fastly's production `select()`, which returns whichever
    /// request completes first and makes no ordering guarantees. Tests that rely on
    /// this stub should not depend on "first-pushed = first-ready" semantics, and
    /// should document their ordering assumptions explicitly if order matters.
    async fn select(
        &self,
        mut pending_requests: Vec<PlatformPendingRequest>,
    ) -> Result<PlatformSelectResult, Report<PlatformError>> {
        if pending_requests.is_empty() {
            return Err(Report::new(PlatformError::HttpClient)
                .attach("select called with empty pending_requests list"));
        }

        let ready_platform = pending_requests.remove(0);
        let stub = ready_platform
            .downcast::<StubPendingResponse>()
            .map_err(|_| {
                Report::new(PlatformError::HttpClient)
                    .attach("unexpected inner type in StubHttpClient::select")
            })?;

        let ready_backend_name = stub.backend_name.clone();

        // Strip backend names from remaining to match Fastly production behavior:
        // Fastly's select() rebuilds remaining with PlatformPendingRequest::new()
        // (no backend_name) — orchestrators must not rely on names being set.
        let remaining: Vec<PlatformPendingRequest> = pending_requests
            .into_iter()
            .map(|r| match r.downcast::<StubPendingResponse>() {
                Ok(inner) => PlatformPendingRequest::new(inner),
                Err(r) => r,
            })
            .collect();

        let edge_response = build_stub_pending_response(stub, false)?;

        let ready = Ok(PlatformResponse::new(edge_response).with_backend_name(ready_backend_name));

        Ok(PlatformSelectResult {
            ready,
            remaining,
            failed_backend_name: None,
        })
    }

    async fn wait(
        &self,
        pending: PlatformPendingRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        let request_is_head = pending.request_method() == Some(&edgezero_core::http::Method::HEAD);
        let stub = pending.downcast::<StubPendingResponse>().map_err(|_| {
            Report::new(PlatformError::HttpClient)
                .attach("unexpected inner type in StubHttpClient::wait")
        })?;
        let backend_name = stub.backend_name.clone();
        let response = build_stub_pending_response(stub, request_is_head)?;
        Ok(PlatformResponse::new(response).with_backend_name(backend_name))
    }
}

// Require the same request marker as Fastly while retaining wrapper metadata.
fn validate_deadline_cache_bypass(
    pending: PlatformPendingRequest,
) -> Result<PlatformPendingRequest, (Option<String>, Report<PlatformError>)> {
    let backend_name = pending.backend_name().map(str::to_owned);
    let request_method = pending.request_method().cloned();
    let stub = pending.downcast::<StubPendingResponse>().map_err(|_| {
        (
            backend_name.clone(),
            Report::new(PlatformError::HttpClient)
                .attach("unexpected pending type in bounded stub collection"),
        )
    })?;
    if !stub.cache_intent.is_bypass() {
        return Err((
            Some(stub.backend_name),
            Report::new(PlatformError::Unsupported)
                .attach("deadline collection requires a cache-bypass request"),
        ));
    }
    let mut pending = PlatformPendingRequest::new(stub);
    if let Some(name) = backend_name {
        pending = pending.with_backend_name(name);
    }
    if let Some(method) = request_method {
        pending = pending.with_response_handling(false, method);
    }
    Ok(pending)
}

fn build_stub_pending_response(
    stub: StubPendingResponse,
    request_is_head: bool,
) -> Result<edgezero_core::http::Response, Report<PlatformError>> {
    let mut builder = edgezero_core::http::response_builder().status(stub.status);
    for (name, value) in stub.headers {
        builder = builder.header(name, value);
    }
    let carries_body = !request_is_head
        && !(100..200).contains(&stub.status)
        && !matches!(stub.status, 204 | 205 | 304);
    let body = if carries_body {
        edgezero_core::body::Body::from(stub.body)
    } else {
        edgezero_core::body::Body::empty()
    };
    builder.body(body).change_context(PlatformError::HttpClient)
}

pub(crate) struct NoopGeo;

impl PlatformGeo for NoopGeo {
    fn lookup(&self, _client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
    }
}

/// Build a [`RuntimeServices`] instance with a custom config store and a custom secret store.
///
/// Use this when a test exercises code that reads from config AND secret stores,
/// such as `request_signing::signing` and `request_signing::rotation`.
pub(crate) fn build_services_with_config_and_secret(
    config_store: impl PlatformConfigStore + 'static,
    secret_store: impl PlatformSecretStore + 'static,
) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(config_store))
        .secret_store(Arc::new(secret_store))
        .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
        .backend(Arc::new(NoopBackend))
        .http_client(Arc::new(NoopHttpClient))
        .outbound_client(ScriptedHttpClient::forbidding().http_client())
        .clock(TestClock::new().monotonic_clock())
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo::default())
        .build()
}

pub(crate) fn build_services_with_config_and_secret_and_client_ip(
    config_store: impl PlatformConfigStore + 'static,
    secret_store: impl PlatformSecretStore + 'static,
    client_ip: IpAddr,
) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(config_store))
        .secret_store(Arc::new(secret_store))
        .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
        .backend(Arc::new(NoopBackend))
        .http_client(Arc::new(NoopHttpClient))
        .outbound_client(ScriptedHttpClient::forbidding().http_client())
        .clock(TestClock::new().monotonic_clock())
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo {
            client_ip: Some(client_ip),
            ..ClientInfo::default()
        })
        .build()
}

pub(crate) fn build_request_signing_services() -> RuntimeServices {
    let signing_key = SigningKey::generate(&mut OsRng);
    let key_b64 = general_purpose::STANDARD.encode(signing_key.as_bytes());
    let x_b64 = general_purpose::URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes());
    let jwk_json =
        format!(r#"{{"kty":"OKP","crv":"Ed25519","x":"{x_b64}","kid":"test-kid","alg":"EdDSA"}}"#);

    let mut config_data = HashMap::new();
    config_data.insert("current-kid".to_owned(), "test-kid".to_owned());
    config_data.insert("test-kid".to_owned(), jwk_json);

    let mut secret_data = HashMap::new();
    secret_data.insert("test-kid".to_owned(), key_b64.into_bytes());

    build_services_with_config_and_secret(
        HashMapConfigStore::new(config_data),
        HashMapSecretStore::new(secret_data),
    )
}

pub(crate) fn build_services_with_config(
    config_store: impl PlatformConfigStore + 'static,
) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(config_store))
        .secret_store(Arc::new(NoopSecretStore))
        .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
        .backend(Arc::new(NoopBackend))
        .http_client(Arc::new(NoopHttpClient))
        .outbound_client(ScriptedHttpClient::forbidding().http_client())
        .clock(TestClock::new().monotonic_clock())
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo::default())
        .build()
}

pub(crate) fn noop_services() -> RuntimeServices {
    build_services_with_config(NoopConfigStore)
}

/// Build test services whose `EdgeZero` outbound requests go to `client`.
///
/// The services clock reads `clock`, so a test that scripts time shares one
/// clock between the scripted replies and the code under test. Every other
/// service is a no-op, so a request through the old platform client fails.
pub(crate) fn build_services_with_outbound_client(
    client: &ScriptedHttpClient,
    clock: &TestClock,
) -> RuntimeServices {
    RuntimeServices {
        outbound_client: client.http_client(),
        clock: clock.monotonic_clock(),
        ..noop_services()
    }
}

/// Build a [`RuntimeServices`] whose auction telemetry sink is the supplied
/// recording (or otherwise custom) sink, so tests can assert which terminal
/// auction events were emitted.
pub(crate) fn noop_services_with_telemetry_sink(
    auction_telemetry_sink: Arc<dyn crate::auction::telemetry::AuctionTelemetrySink>,
) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(NoopConfigStore))
        .secret_store(Arc::new(NoopSecretStore))
        .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
        .backend(Arc::new(NoopBackend))
        .http_client(Arc::new(NoopHttpClient))
        .outbound_client(ScriptedHttpClient::forbidding().http_client())
        .clock(TestClock::new().monotonic_clock())
        .geo(Arc::new(NoopGeo))
        .auction_telemetry_sink(auction_telemetry_sink)
        .client_info(ClientInfo::default())
        .build()
}

/// Build a [`RuntimeServices`] with a caller-supplied HTTP client and a [`StubBackend`].
///
/// Uses [`StubBackend`] (always returns `Ok("stub-backend")`) rather than
/// [`NoopBackend`] (always returns `Err(Unsupported)`) so that handlers which
/// both make HTTP calls and resolve backends don't need two separate service
/// setups.  If your test must verify that a missing backend returns an error,
/// use [`noop_services`] directly.
pub(crate) fn build_services_with_http_client(
    http_client: Arc<dyn PlatformHttpClient>,
) -> RuntimeServices {
    build_services_with_config_secret_and_http_client(NoopConfigStore, NoopSecretStore, http_client)
}

pub(crate) fn noop_services_with_client_ip(ip: IpAddr) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(NoopConfigStore))
        .secret_store(Arc::new(NoopSecretStore))
        .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
        .backend(Arc::new(NoopBackend))
        .http_client(Arc::new(NoopHttpClient))
        .outbound_client(ScriptedHttpClient::forbidding().http_client())
        .clock(TestClock::new().monotonic_clock())
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo {
            client_ip: Some(ip),
            ..ClientInfo::default()
        })
        .build()
}

/// Build a [`RuntimeServices`] with a custom secret store, [`StubBackend`], and HTTP client.
pub(crate) fn build_services_with_config_secret_and_http_client(
    config_store: impl PlatformConfigStore + 'static,
    secret_store: impl PlatformSecretStore + 'static,
    http_client: Arc<dyn PlatformHttpClient>,
) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(config_store))
        .secret_store(Arc::new(secret_store))
        .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
        .backend(Arc::new(StubBackend))
        .http_client(http_client)
        .outbound_client(ScriptedHttpClient::forbidding().http_client())
        .clock(TestClock::new().monotonic_clock())
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo::default())
        .build()
}

#[cfg(test)]
mod tests {
    use crate::platform::DEFAULT_FIRST_BYTE_TIMEOUT;
    use edgezero_core::body::Body;
    use edgezero_core::http::request_builder;

    use super::*;

    fn dispatch_cache_test_request(
        stub: &StubHttpClient,
        backend: &str,
        bypass: bool,
    ) -> PlatformPendingRequest {
        stub.push_response(200, b"ready".to_vec());
        let mut request = PlatformHttpRequest::new(
            request_builder()
                .uri("https://example.com/bid")
                .body(Body::empty())
                .expect("should build request"),
            backend,
        );
        if bypass {
            request = request.with_cache_bypass();
        }
        futures::executor::block_on(stub.send_async(request)).expect("should dispatch request")
    }

    #[test]
    fn bounded_stub_collection_rejects_missing_bypass_without_losing_siblings() {
        for unsupported_first in [false, true] {
            let stub = StubHttpClient::new();
            let unsupported = dispatch_cache_test_request(&stub, "unsupported", false);
            let healthy = dispatch_cache_test_request(&stub, "healthy", true);
            let selected = futures::executor::block_on(stub.select_until(
                if unsupported_first {
                    vec![unsupported, healthy]
                } else {
                    vec![healthy, unsupported]
                },
                Instant::now(),
            ))
            .expect("should attribute unsupported request")
            .expect("should return unsupported result");
            assert!(
                matches!(
                    selected
                        .ready
                        .expect_err("should reject missing bypass")
                        .current_context(),
                    PlatformError::Unsupported
                ),
                "should match Fastly's cache bypass requirement"
            );
            assert_eq!(
                selected.failed_backend_name.as_deref(),
                Some("unsupported"),
                "should identify unsupported provider"
            );
            let selected =
                futures::executor::block_on(stub.select_until(selected.remaining, Instant::now()))
                    .expect("should collect sibling")
                    .expect("should retain ready sibling");
            assert_eq!(
                selected
                    .ready
                    .expect("should accept bypassed request")
                    .backend_name
                    .as_deref(),
                Some("healthy"),
                "should preserve healthy provider"
            );
        }
    }

    #[test]
    fn bounded_stub_wait_rejects_missing_bypass() {
        let stub = StubHttpClient::new();
        let pending = dispatch_cache_test_request(&stub, "unsupported", false);
        let error = futures::executor::block_on(stub.wait_until(pending, Instant::now()))
            .expect_err("should reject missing bypass");
        assert!(
            matches!(error.current_context(), PlatformError::Unsupported),
            "should reject unsupported deadline wait"
        );
    }

    #[test]
    fn unbounded_stub_wait_accepts_default_cache_intent() {
        let stub = StubHttpClient::new();
        let pending = dispatch_cache_test_request(&stub, "ordinary", false);
        let response = futures::executor::block_on(stub.wait(pending))
            .expect("should preserve ordinary waits");
        assert_eq!(
            response.backend_name.as_deref(),
            Some("ordinary"),
            "should preserve backend"
        );
    }

    #[test]
    fn bounded_stub_wait_preserves_an_already_ready_response_after_expiry() {
        let stub = StubHttpClient::new();
        stub.push_response(200, b"ready".to_vec());
        let pending = futures::executor::block_on(
            stub.send_async(
                PlatformHttpRequest::new(
                    request_builder()
                        .uri("https://example.com/ready")
                        .body(Body::empty())
                        .expect("should build request"),
                    "ready-backend",
                )
                .with_cache_bypass(),
            ),
        )
        .expect("should dispatch ready response");
        let response = futures::executor::block_on(stub.wait_until(pending, Instant::now()))
            .expect("should retain the ready response after expiry");
        assert_eq!(
            response.response.status().as_u16(),
            200,
            "should preserve ready status"
        );
    }

    #[test]
    fn stub_http_client_records_send_calls_and_returns_canned_response() {
        let stub = StubHttpClient::new();
        stub.push_response(200, b"hello".to_vec());

        let req = PlatformHttpRequest::new(
            request_builder()
                .method("GET")
                .uri("https://example.com/test")
                .body(Body::empty())
                .expect("should build request"),
            "stub-backend",
        );
        let result = futures::executor::block_on(stub.send(req));

        assert!(result.is_ok(), "should return canned response");
        let names = stub.recorded_backend_names();
        assert_eq!(
            names,
            vec!["stub-backend"],
            "should record the backend name"
        );
    }

    #[test]
    fn stub_http_client_returns_error_when_no_response_queued() {
        let stub = StubHttpClient::new();

        let req = PlatformHttpRequest::new(
            request_builder()
                .method("GET")
                .uri("https://example.com/")
                .body(Body::empty())
                .expect("should build request"),
            "stub-backend",
        );
        let result = futures::executor::block_on(stub.send(req));

        assert!(result.is_err(), "should return error when queue is empty");
        assert!(
            matches!(
                result.unwrap_err().current_context(),
                PlatformError::HttpClient
            ),
            "should be HttpClient error"
        );
    }

    #[test]
    fn stub_http_client_send_async_and_select_fan_out() {
        let stub = StubHttpClient::new();
        stub.push_response(200, b"provider-a".to_vec());
        stub.push_response(201, b"provider-b".to_vec());

        let make_req = |backend: &str| {
            PlatformHttpRequest::new(
                request_builder()
                    .method("GET")
                    .uri("https://example.com/bid")
                    .body(Body::empty())
                    .expect("should build request"),
                backend,
            )
        };

        let pending_a = futures::executor::block_on(stub.send_async(make_req("backend-a")))
            .expect("should start request a");
        let pending_b =
            futures::executor::block_on(stub.send_async(make_req("backend-b").with_cache_bypass()))
                .expect("should start request b");

        assert_eq!(
            pending_a.backend_name(),
            Some("backend-a"),
            "should attach backend name to pending request a"
        );
        assert_eq!(
            pending_b.backend_name(),
            Some("backend-b"),
            "should attach backend name to pending request b"
        );

        let result = futures::executor::block_on(stub.select(vec![pending_a, pending_b]))
            .expect("should select first ready request");

        let ready_resp = result.ready.expect("should have a ready response");
        assert_eq!(
            ready_resp.backend_name.as_deref(),
            Some("backend-a"),
            "should correlate ready response to backend-a"
        );
        assert_eq!(
            result.remaining.len(),
            1,
            "should have one remaining request"
        );
        assert_eq!(
            result.remaining[0].backend_name(),
            None,
            "should strip backend name from remaining (matches Fastly production behavior)"
        );

        let names = stub.recorded_backend_names();
        assert_eq!(
            names,
            vec!["backend-a", "backend-b"],
            "should record both send_async calls in order"
        );
    }

    #[test]
    fn stub_http_client_select_returns_error_when_empty() {
        let stub = StubHttpClient::new();
        let err = futures::executor::block_on(stub.select(vec![]))
            .expect_err("should return error for empty list");
        assert!(
            matches!(err.current_context(), PlatformError::HttpClient),
            "should be HttpClient error"
        );
    }

    #[test]
    fn stub_backend_returns_fixed_name() {
        let stub = StubBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_owned(),
            host: "example.com".to_owned(),
            port: None,
            host_header_override: None,
            certificate_check: true,
            first_byte_timeout: DEFAULT_FIRST_BYTE_TIMEOUT,
            between_bytes_timeout: DEFAULT_FIRST_BYTE_TIMEOUT,
            discriminator: None,
        };
        let name = stub.ensure(&spec).expect("should return a backend name");
        assert_eq!(name, "stub-backend", "should return fixed name");
    }

    #[test]
    fn build_services_with_config_and_secret_uses_provided_stores() {
        // Arrange: noop stores
        let services = build_services_with_config_and_secret(NoopConfigStore, NoopSecretStore);

        // Act: both stores return Unsupported (confirming the injected impls are active)
        let config_result = services.config_store().get(&StoreName::from("s"), "k");
        let secret_result = services
            .secret_store()
            .get_bytes(&StoreName::from("s"), "k");

        assert!(
            config_result.is_err(),
            "should delegate to injected config store"
        );
        assert!(
            secret_result.is_err(),
            "should delegate to injected secret store"
        );
    }

    #[test]
    fn hash_map_stores_return_preset_values() {
        let mut config = HashMap::new();
        config.insert("current-kid".to_owned(), "test-kid".to_owned());

        let mut secrets = HashMap::new();
        secrets.insert("test-kid".to_owned(), b"secret-material".to_vec());

        let services = build_services_with_config_and_secret(
            HashMapConfigStore::new(config),
            HashMapSecretStore::new(secrets),
        );

        assert_eq!(
            services
                .config_store()
                .get(&JWKS_STORE_NAME, "current-kid")
                .expect("should read current-kid from config test store"),
            "test-kid"
        );
        assert_eq!(
            services
                .secret_store()
                .get_bytes(&SIGNING_STORE_NAME, "test-kid")
                .expect("should read signing key bytes from secret test store"),
            b"secret-material".to_vec()
        );
    }

    #[test]
    fn build_request_signing_services_provides_current_kid_and_signing_key() {
        let services = build_request_signing_services();

        let kid = services
            .config_store()
            .get(&JWKS_STORE_NAME, "current-kid")
            .expect("should expose current-kid in config store");
        let key_bytes = services
            .secret_store()
            .get_bytes(&SIGNING_STORE_NAME, &kid)
            .expect("should expose signing key bytes in secret store");

        assert_eq!(kid, "test-kid", "should use the standard signing test kid");
        assert!(
            !key_bytes.is_empty(),
            "should provide key material for the current signing key"
        );
    }
}
