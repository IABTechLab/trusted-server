//! Fastly-backed implementations of the platform traits defined in
//! `trusted-server-core::platform`.

use std::net::IpAddr;
use std::sync::Arc;

use edgezero_adapter_fastly::key_value_store::FastlyKvStore;
use edgezero_core::key_value_store::KvError;
use error_stack::{Report, ResultExt};
use fastly::geo::{geo_lookup, Geo};
use fastly::{ConfigStore, SecretStore};

use crate::backend::BackendConfig;
pub(crate) use trusted_server_core::platform::UnavailableKvStore;
use trusted_server_core::platform::{
    GeoInfo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore, PlatformError, PlatformGeo,
    PlatformHttpClient, PlatformHttpRequest, PlatformKvStore, PlatformPendingRequest,
    PlatformResponse, PlatformSecretStore, PlatformSelectResult, StoreId, StoreName,
};

// ---------------------------------------------------------------------------
// FastlyPlatformConfigStore
// ---------------------------------------------------------------------------

/// Fastly [`ConfigStore`]-backed implementation of [`PlatformConfigStore`].
///
/// Stateless — the store name is supplied per call, matching the trait
/// signature. This replaces the store-name-at-construction pattern of
/// the legacy `FastlyConfigStore` (removed).
///
/// # Write cost
///
/// `put` and `delete` each perform a synchronous outbound HTTPS request to the
/// Fastly management API (`api.fastly.com`). Callers that issue many writes in
/// one request pay one round-trip per call. The `"api-keys"` secret store is
/// opened per call to read the management token; the Fastly Compute SDK caches
/// the open handle so that cost is negligible.
pub struct FastlyPlatformConfigStore;

impl PlatformConfigStore for FastlyPlatformConfigStore {
    fn get(&self, store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        let name = store_name.as_ref();
        let store = ConfigStore::try_open(name).map_err(|e| {
            Report::new(PlatformError::ConfigStore)
                .attach(format!("failed to open config store '{name}': {e}"))
        })?;
        store
            .try_get(key)
            .map_err(|e| {
                Report::new(PlatformError::ConfigStore).attach(format!(
                    "lookup for key '{key}' in config store '{name}' failed: {e}"
                ))
            })?
            .ok_or_else(|| {
                Report::new(PlatformError::ConfigStore)
                    .attach(format!("key '{key}' not found in config store '{name}'"))
            })
    }

    fn put(&self, store_id: &StoreId, key: &str, value: &str) -> Result<(), Report<PlatformError>> {
        let client = crate::management_api::FastlyManagementApiClient::new()?;
        client.update_config_item(store_id.as_ref(), key, value)
    }

    fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
        let client = crate::management_api::FastlyManagementApiClient::new()?;
        client.delete_config_item(store_id.as_ref(), key)
    }
}

// ---------------------------------------------------------------------------
// FastlyPlatformSecretStore
// ---------------------------------------------------------------------------

/// Fastly [`SecretStore`]-backed implementation of [`PlatformSecretStore`].
///
/// Stateless — the store name is supplied per call. This replaces the
/// store-name-at-construction pattern of the legacy `FastlySecretStore`
/// (removed).
///
/// # Write cost
///
/// `create` and `delete` have the same per-call
/// [`crate::management_api::FastlyManagementApiClient`] cost described on
/// [`FastlyPlatformConfigStore`].
pub struct FastlyPlatformSecretStore;

impl PlatformSecretStore for FastlyPlatformSecretStore {
    fn get_bytes(
        &self,
        store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        let name = store_name.as_ref();
        // Unlike ConfigStore::open (which panics), SecretStore::open already
        // returns Result — there is no try_open variant on SecretStore.
        let store = SecretStore::open(name).map_err(|e| {
            Report::new(PlatformError::SecretStore)
                .attach(format!("failed to open secret store '{name}': {e}"))
        })?;
        let secret = store
            .try_get(key)
            .map_err(|e| {
                Report::new(PlatformError::SecretStore).attach(format!(
                    "lookup for key '{key}' in secret store '{name}' failed: {e}"
                ))
            })?
            .ok_or_else(|| {
                Report::new(PlatformError::SecretStore)
                    .attach(format!("key '{key}' not found in secret store '{name}'"))
            })?;
        secret
            .try_plaintext()
            .map(|bytes| bytes.to_vec())
            .map_err(|e| {
                Report::new(PlatformError::SecretStore)
                    .attach(format!("failed to decrypt secret '{key}': {e}"))
            })
    }

    fn create(
        &self,
        store_id: &StoreId,
        name: &str,
        value: &str,
    ) -> Result<(), Report<PlatformError>> {
        let client = crate::management_api::FastlyManagementApiClient::new()?;
        client.create_secret(store_id.as_ref(), name, value)
    }

    fn delete(&self, store_id: &StoreId, name: &str) -> Result<(), Report<PlatformError>> {
        let client = crate::management_api::FastlyManagementApiClient::new()?;
        client.delete_secret(store_id.as_ref(), name)
    }
}

// ---------------------------------------------------------------------------
// FastlyPlatformBackend
// ---------------------------------------------------------------------------

/// Fastly dynamic-backend implementation of [`PlatformBackend`].
///
/// Delegates name computation and registration to [`BackendConfig`], preserving
/// the existing deterministic naming scheme (scheme + host + port + cert +
/// timeout → unique name).
pub struct FastlyPlatformBackend;

fn backend_config_from_spec(spec: &PlatformBackendSpec) -> BackendConfig<'_> {
    BackendConfig::new(&spec.scheme, &spec.host)
        .port(spec.port)
        .certificate_check(spec.certificate_check)
        .first_byte_timeout(spec.first_byte_timeout)
}

impl PlatformBackend for FastlyPlatformBackend {
    fn predict_name(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        backend_config_from_spec(spec)
            .predict_name()
            .change_context(PlatformError::Backend)
    }

    fn ensure(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        backend_config_from_spec(spec)
            .ensure()
            .change_context(PlatformError::Backend)
    }
}

// ---------------------------------------------------------------------------
// FastlyPlatformHttpClient — helpers
// ---------------------------------------------------------------------------

/// Convert a platform-neutral [`edgezero_core::http::Request`] to a [`fastly::Request`].
///
/// Only buffered `Body::Once` bodies are supported on this path.
///
/// # Errors
///
/// Returns [`PlatformError::HttpClient`] when the request body is streaming.
fn edge_request_to_fastly(
    request: edgezero_core::http::Request,
) -> Result<fastly::Request, Report<PlatformError>> {
    let (parts, body) = request.into_parts();
    let mut fastly_req = fastly::Request::new(parts.method, parts.uri.to_string());
    for (name, value) in parts.headers.iter() {
        fastly_req.append_header(name.as_str(), value.as_bytes());
    }
    match body {
        edgezero_core::body::Body::Once(bytes) => {
            if !bytes.is_empty() {
                fastly_req.set_body(bytes.to_vec());
            }
        }
        edgezero_core::body::Body::Stream(_) => {
            return Err(Report::new(PlatformError::HttpClient)
                .attach("streaming request body is not supported by Fastly request conversion"));
        }
    }
    Ok(fastly_req)
}

/// Convert a [`fastly::Response`] to a [`PlatformResponse`] with the given backend name.
fn fastly_response_to_platform(
    mut resp: fastly::Response,
    backend_name: impl Into<String>,
) -> Result<PlatformResponse, Report<PlatformError>> {
    let status = resp.get_status();
    let mut builder = edgezero_core::http::response_builder().status(status);
    for (name, value) in resp.get_headers() {
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let body_bytes = resp.take_body_bytes();
    let edge_response = builder
        .body(edgezero_core::body::Body::from(body_bytes))
        .change_context(PlatformError::HttpClient)?;
    Ok(PlatformResponse::new(edge_response).with_backend_name(backend_name))
}

// ---------------------------------------------------------------------------
// FastlyPlatformHttpClient
// ---------------------------------------------------------------------------

/// Fastly implementation of [`PlatformHttpClient`].
///
/// - [`send`](PlatformHttpClient::send) — converts the platform request to a
///   `fastly::Request`, calls `.send()`, and wraps the response.
/// - [`send_async`](PlatformHttpClient::send_async) — same conversion but
///   calls `.send_async()` and wraps the `fastly::PendingRequest`.
/// - [`select`](PlatformHttpClient::select) — downcasts each
///   [`PlatformPendingRequest`] back to `fastly::PendingRequest` and calls
///   `fastly::http::request::select()`.
pub struct FastlyPlatformHttpClient;

#[async_trait::async_trait(?Send)]
impl PlatformHttpClient for FastlyPlatformHttpClient {
    async fn send(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        let backend_name = request.backend_name.clone();
        let fastly_req = edge_request_to_fastly(request.request)?;
        let fastly_resp = fastly_req
            .send(&backend_name)
            .change_context(PlatformError::HttpClient)?;
        fastly_response_to_platform(fastly_resp, backend_name)
    }

    async fn send_async(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
        let backend_name = request.backend_name.clone();
        let fastly_req = edge_request_to_fastly(request.request)?;
        let pending = fastly_req
            .send_async(&backend_name)
            .change_context(PlatformError::HttpClient)?;
        Ok(PlatformPendingRequest::new(pending).with_backend_name(backend_name))
    }

    async fn select(
        &self,
        pending_requests: Vec<PlatformPendingRequest>,
    ) -> Result<PlatformSelectResult, Report<PlatformError>> {
        use fastly::http::request::{select, PendingRequest};

        if pending_requests.is_empty() {
            return Err(Report::new(PlatformError::HttpClient)
                .attach("select called with an empty pending_requests list"));
        }

        let mut fastly_pending: Vec<PendingRequest> = Vec::with_capacity(pending_requests.len());

        for platform_req in pending_requests {
            let inner = platform_req.downcast::<PendingRequest>().map_err(|platform_req| {
                let backend_name = platform_req.backend_name().unwrap_or("<unknown>");
                Report::new(PlatformError::HttpClient).attach(format!(
                    "PlatformPendingRequest inner type is not fastly::PendingRequest for backend '{backend_name}'"
                ))
            })?;
            fastly_pending.push(inner);
        }

        let (result, remaining_fastly) = select(fastly_pending);

        // Fastly's select() does not preserve input order for remaining requests,
        // so positional backend-name re-association is unreliable. Backend names
        // are re-derived from get_backend_name() when each remaining request completes.
        let remaining: Vec<PlatformPendingRequest> = remaining_fastly
            .into_iter()
            .map(PlatformPendingRequest::new)
            .collect();

        let ready = match result {
            Ok(fastly_resp) => {
                let backend_name = fastly_resp
                    .get_backend_name()
                    .unwrap_or_else(|| {
                        log::warn!("select: response has no backend name, correlation will fail");
                        ""
                    })
                    .to_string();
                fastly_response_to_platform(fastly_resp, backend_name)
            }
            Err(e) => {
                Err(Report::new(PlatformError::HttpClient)
                    .attach(format!("fastly select error: {e}")))
            }
        };

        Ok(PlatformSelectResult { ready, remaining })
    }
}

// ---------------------------------------------------------------------------
// FastlyPlatformGeo
// ---------------------------------------------------------------------------

/// Convert a Fastly [`Geo`] value into a platform-neutral [`GeoInfo`].
///
/// Shared by `FastlyPlatformGeo::lookup` in `trusted-server-adapter-fastly` so
/// that field mapping is never duplicated.
fn geo_from_fastly(geo: &Geo) -> GeoInfo {
    GeoInfo {
        city: geo.city().to_string(),
        country: geo.country_code().to_string(),
        continent: format!("{:?}", geo.continent()),
        latitude: geo.latitude(),
        longitude: geo.longitude(),
        metro_code: geo.metro_code(),
        region: geo.region().map(str::to_string),
    }
}

/// Fastly geo-lookup implementation of [`PlatformGeo`].
pub struct FastlyPlatformGeo;

impl PlatformGeo for FastlyPlatformGeo {
    fn lookup(&self, client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(client_ip
            .and_then(geo_lookup)
            .map(|geo| geo_from_fastly(&geo)))
    }
}

// ---------------------------------------------------------------------------
// KV store
// ---------------------------------------------------------------------------

/// Open a named KV store as a [`PlatformKvStore`] implementation.
///
/// # Errors
///
/// Returns [`KvError::Unavailable`] when the store does not exist, or
/// [`KvError::Internal`] when the Fastly SDK fails to open it.
pub fn open_kv_store(store_name: &str) -> Result<Arc<dyn PlatformKvStore>, KvError> {
    FastlyKvStore::open(store_name).map(|store| Arc::new(store) as Arc<dyn PlatformKvStore>)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io;
    use std::time::Duration;

    use edgezero_core::body::Body;
    use edgezero_core::http::request_builder;

    use super::*;

    // --- FastlyPlatformBackend::predict_name --------------------------------

    #[test]
    fn predict_name_produces_same_name_as_backend_config() {
        let backend = FastlyPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "origin.example.com".to_string(),
            port: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_secs(15),
        };

        let name = backend
            .predict_name(&spec)
            .expect("should compute backend name for valid spec");

        assert_eq!(
            name, "backend_https_origin_example_com_443_t15000",
            "should match BackendConfig naming convention"
        );
    }

    #[test]
    fn predict_name_includes_nocert_suffix_when_cert_check_disabled() {
        let backend = FastlyPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "origin.example.com".to_string(),
            port: None,
            certificate_check: false,
            first_byte_timeout: Duration::from_secs(15),
        };

        let name = backend
            .predict_name(&spec)
            .expect("should compute name with cert check disabled");

        assert!(
            name.contains("nocert"),
            "should include nocert suffix when certificate_check is false"
        );
    }

    #[test]
    fn predict_name_returns_error_for_empty_host() {
        let backend = FastlyPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: String::new(),
            port: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_secs(15),
        };

        let result = backend.predict_name(&spec);

        assert!(result.is_err(), "should return an error for empty host");
    }

    #[test]
    fn predict_name_encodes_custom_timeout() {
        let backend = FastlyPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "origin.example.com".to_string(),
            port: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_millis(2000),
        };

        let name = backend
            .predict_name(&spec)
            .expect("should compute name with custom timeout");

        assert!(
            name.ends_with("_t2000"),
            "should encode 2000ms timeout in name"
        );
    }

    // --- FastlyPlatformHttpClient -------------------------------------------

    #[test]
    fn fastly_platform_http_client_send_returns_error_for_unregistered_backend() {
        let client = FastlyPlatformHttpClient;
        let request = request_builder()
            .method("GET")
            .uri("https://example.com/")
            .body(Body::empty())
            .expect("should build test request");
        let err = futures::executor::block_on(
            client.send(PlatformHttpRequest::new(request, "nonexistent-backend")),
        )
        .expect_err("should return error for unregistered backend");

        assert!(
            matches!(err.current_context(), &PlatformError::HttpClient),
            "should be HttpClient error, got: {:?}",
            err.current_context()
        );
    }

    #[test]
    fn fastly_platform_http_client_send_async_returns_error_for_unregistered_backend() {
        let client = FastlyPlatformHttpClient;
        let request = request_builder()
            .method("GET")
            .uri("https://example.com/")
            .body(Body::empty())
            .expect("should build test request");
        let err = futures::executor::block_on(
            client.send_async(PlatformHttpRequest::new(request, "nonexistent-backend")),
        )
        .expect_err("should return error for unregistered backend");

        assert!(
            matches!(err.current_context(), &PlatformError::HttpClient),
            "should be HttpClient error, got: {:?}",
            err.current_context()
        );
    }

    #[test]
    fn fastly_platform_http_client_select_returns_error_for_empty_list() {
        let client = FastlyPlatformHttpClient;
        let err = futures::executor::block_on(client.select(vec![]))
            .expect_err("should return error for empty pending list");

        assert!(
            matches!(err.current_context(), &PlatformError::HttpClient),
            "should be HttpClient error, got: {:?}",
            err.current_context()
        );
    }

    #[test]
    fn fastly_platform_http_client_select_returns_error_for_wrong_inner_type() {
        let client = FastlyPlatformHttpClient;
        // Wrap a non-PendingRequest type to trigger the downcast failure.
        let wrong = PlatformPendingRequest::new(42u32).with_backend_name("origin-a");
        let err = futures::executor::block_on(client.select(vec![wrong]))
            .expect_err("should return error for wrong inner type");

        assert!(
            matches!(err.current_context(), &PlatformError::HttpClient),
            "should be HttpClient error, got: {:?}",
            err.current_context()
        );
        assert!(
            format!("{err:?}").contains("origin-a"),
            "should include backend name in error report: {err:?}"
        );
    }

    #[test]
    fn fastly_platform_http_client_send_returns_error_for_streaming_body() {
        let client = FastlyPlatformHttpClient;
        let request = request_builder()
            .method("POST")
            .uri("https://example.com/")
            .body(Body::from_stream(futures::stream::empty::<
                Result<_, io::Error>,
            >()))
            .expect("should build streaming test request");

        let err = futures::executor::block_on(
            client.send(PlatformHttpRequest::new(request, "nonexistent-backend")),
        )
        .expect_err("should reject streaming request bodies before sending");

        assert!(
            matches!(err.current_context(), &PlatformError::HttpClient),
            "should be HttpClient error, got: {:?}",
            err.current_context()
        );
        assert!(
            format!("{err:?}").contains("streaming request body"),
            "should describe the unsupported streaming body: {err:?}"
        );
    }

    #[test]
    fn fastly_platform_http_client_send_async_returns_error_for_streaming_body() {
        let client = FastlyPlatformHttpClient;
        let request = request_builder()
            .method("POST")
            .uri("https://example.com/")
            .body(Body::from_stream(futures::stream::empty::<
                Result<_, io::Error>,
            >()))
            .expect("should build streaming test request");

        let err = futures::executor::block_on(
            client.send_async(PlatformHttpRequest::new(request, "nonexistent-backend")),
        )
        .expect_err("should reject streaming request bodies before launching async send");

        assert!(
            matches!(err.current_context(), &PlatformError::HttpClient),
            "should be HttpClient error, got: {:?}",
            err.current_context()
        );
        assert!(
            format!("{err:?}").contains("streaming request body"),
            "should describe the unsupported streaming body: {err:?}"
        );
    }
}
