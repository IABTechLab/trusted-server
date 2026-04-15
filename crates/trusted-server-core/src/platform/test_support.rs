use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use error_stack::{Report, ResultExt};

use super::{
    ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore, PlatformError,
    PlatformGeo, PlatformHttpClient, PlatformHttpRequest, PlatformPendingRequest, PlatformResponse,
    PlatformSecretStore, PlatformSelectResult, RuntimeServices, StoreId, StoreName,
};

pub(crate) struct NoopConfigStore;

impl PlatformConfigStore for NoopConfigStore {
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

pub(crate) struct NoopSecretStore;

impl PlatformSecretStore for NoopSecretStore {
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

pub(crate) struct NoopBackend;

impl PlatformBackend for NoopBackend {
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
    fn predict_name(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Ok("stub-backend".to_string())
    }

    fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Ok("stub-backend".to_string())
    }
}

// ---------------------------------------------------------------------------
// StubHttpClient
// ---------------------------------------------------------------------------

/// Canned response carried by a [`PlatformPendingRequest`] through `send_async`
/// and resolved by [`StubHttpClient::select`].
struct StubPendingResponse {
    backend_name: String,
    status: u16,
    body: Vec<u8>,
}

/// Test stub for [`PlatformHttpClient`] that records call backend names and
/// returns pre-queued canned responses for `send`, `send_async`, and `select`.
///
/// Responses are stored as `(status_code, body_bytes)` to remain [`Send`].
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
    // (status_code, body_bytes) — kept Send by avoiding Body::Stream
    responses: Mutex<VecDeque<(u16, Vec<u8>)>>,
    // Headers captured per send call, stored as (name, value) string pairs.
    request_headers: Mutex<Vec<Vec<(String, String)>>>,
}

impl StubHttpClient {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            responses: Mutex::new(VecDeque::new()),
            request_headers: Mutex::new(Vec::new()),
        }
    }

    /// Queue a canned response by status code and body bytes.
    pub fn push_response(&self, status: u16, body: Vec<u8>) {
        self.responses
            .lock()
            .expect("should lock responses")
            .push_back((status, body));
    }

    /// Return backend names recorded across all `send` calls, in order.
    pub fn recorded_backend_names(&self) -> Vec<String> {
        self.calls.lock().expect("should lock calls").clone()
    }

    /// Return the request headers captured per `send` call, in order.
    ///
    /// Each entry is the set of `(name, value)` pairs from one call.
    pub fn recorded_request_headers(&self) -> Vec<Vec<(String, String)>> {
        self.request_headers
            .lock()
            .expect("should lock request_headers")
            .clone()
    }
}

// ?Send matches PlatformHttpClient. See http.rs for the full rationale.
#[async_trait::async_trait(?Send)]
impl PlatformHttpClient for StubHttpClient {
    async fn send(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        self.calls
            .lock()
            .expect("should lock calls")
            .push(request.backend_name.clone());

        let headers: Vec<(String, String)> = request
            .request
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|v| (name.as_str().to_string(), v.to_string()))
            })
            .collect();
        self.request_headers
            .lock()
            .expect("should lock request_headers")
            .push(headers);

        let (status, body_bytes) = self
            .responses
            .lock()
            .expect("should lock responses")
            .pop_front()
            .ok_or_else(|| Report::new(PlatformError::HttpClient))?;

        let edge_response = edgezero_core::http::response_builder()
            .status(status)
            .body(edgezero_core::body::Body::from(body_bytes))
            .change_context(PlatformError::HttpClient)?;

        Ok(PlatformResponse::new(edge_response))
    }

    async fn send_async(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
        let backend_name = request.backend_name.clone();
        self.calls
            .lock()
            .expect("should lock calls")
            .push(backend_name.clone());

        let (status, body_bytes) = self
            .responses
            .lock()
            .expect("should lock responses")
            .pop_front()
            .ok_or_else(|| Report::new(PlatformError::HttpClient))?;

        let pending = StubPendingResponse {
            backend_name: backend_name.clone(),
            status,
            body: body_bytes,
        };
        Ok(PlatformPendingRequest::new(pending).with_backend_name(backend_name))
    }

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

        let edge_response = edgezero_core::http::response_builder()
            .status(stub.status)
            .body(edgezero_core::body::Body::from(stub.body))
            .change_context(PlatformError::HttpClient)?;

        let ready = Ok(PlatformResponse::new(edge_response).with_backend_name(stub.backend_name));

        Ok(PlatformSelectResult {
            ready,
            remaining: pending_requests,
        })
    }
}

pub(crate) struct NoopGeo;

impl PlatformGeo for NoopGeo {
    fn lookup(&self, _client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
    }
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
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo {
            client_ip: None,
            tls_protocol: None,
            tls_cipher: None,
        })
        .build()
}

pub(crate) fn noop_services() -> RuntimeServices {
    build_services_with_config(NoopConfigStore)
}

pub(crate) fn noop_services_with_client_ip(ip: IpAddr) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(NoopConfigStore))
        .secret_store(Arc::new(NoopSecretStore))
        .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
        .backend(Arc::new(NoopBackend))
        .http_client(Arc::new(NoopHttpClient))
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo {
            client_ip: Some(ip),
            tls_protocol: None,
            tls_cipher: None,
        })
        .build()
}

/// Build a [`RuntimeServices`] with a [`StubBackend`] and the given HTTP client.
///
/// Useful for tests that need to verify `services.http_client()` call sites.
pub(crate) fn build_services_with_http_client(
    http_client: Arc<dyn PlatformHttpClient>,
) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(NoopConfigStore))
        .secret_store(Arc::new(NoopSecretStore))
        .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
        .backend(Arc::new(StubBackend))
        .http_client(http_client)
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo {
            client_ip: None,
            tls_protocol: None,
            tls_cipher: None,
        })
        .build()
}

#[cfg(test)]
mod tests {
    use crate::backend::DEFAULT_FIRST_BYTE_TIMEOUT;
    use edgezero_core::body::Body;
    use edgezero_core::http::request_builder;

    use super::*;

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
        let pending_b = futures::executor::block_on(stub.send_async(make_req("backend-b")))
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
            Some("backend-b"),
            "should preserve backend name on remaining request"
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
            scheme: "https".to_string(),
            host: "example.com".to_string(),
            port: None,
            certificate_check: true,
            first_byte_timeout: DEFAULT_FIRST_BYTE_TIMEOUT,
        };
        let name = stub.ensure(&spec).expect("should return a backend name");
        assert_eq!(name, "stub-backend", "should return fixed name");
    }
}
