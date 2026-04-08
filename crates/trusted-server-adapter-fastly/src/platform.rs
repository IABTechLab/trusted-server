//! Fastly-backed implementations of the platform traits defined in
//! `trusted-server-core::platform`.
//!
//! This module also provides [`build_runtime_services`], a free function that
//! constructs a [`RuntimeServices`] instance once at the entry point from the
//! incoming Fastly request.

use std::net::IpAddr;
use std::sync::Arc;

use edgezero_adapter_fastly::key_value_store::FastlyKvStore;
use edgezero_core::key_value_store::KvError;
use error_stack::{Report, ResultExt};
use fastly::geo::geo_lookup;
use fastly::{ConfigStore, Request, SecretStore};

use trusted_server_core::backend::BackendConfig;
use trusted_server_core::geo::geo_from_fastly;
pub(crate) use trusted_server_core::platform::UnavailableKvStore;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore, PlatformError,
    PlatformGeo, PlatformHttpClient, PlatformHttpRequest, PlatformKvStore, PlatformPendingRequest,
    PlatformResponse, PlatformSecretStore, PlatformSelectResult, RuntimeServices, StoreId,
    StoreName,
};

// ---------------------------------------------------------------------------
// FastlyPlatformConfigStore
// ---------------------------------------------------------------------------

/// Fastly [`ConfigStore`]-backed implementation of [`PlatformConfigStore`].
///
/// Stateless — the store name is supplied per call, matching the trait
/// signature. This replaces the store-name-at-construction pattern of
/// [`trusted_server_core::storage::FastlyConfigStore`].
///
/// # Write cost
///
/// `put` and `delete` construct a
/// [`crate::management_api::FastlyManagementApiClient`] on every call, which
/// opens the `"api-keys"` secret store to read the management API key. On
/// Fastly Compute, the SDK caches the open handle so repeated opens within a
/// single request are cheap. Callers that issue many writes in one request
/// should be aware that each call performs a synchronous outbound API request
/// to the Fastly management API.
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
/// store-name-at-construction pattern of
/// [`trusted_server_core::storage::FastlySecretStore`].
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
/// Only `Body::Once` bodies are forwarded; `Body::Stream` bodies are not
/// used on this path (proxy.rs builds bodies from byte slices).
fn edge_request_to_fastly(request: edgezero_core::http::Request) -> fastly::Request {
    let (parts, body) = request.into_parts();
    let mut fastly_req = fastly::Request::new(parts.method, parts.uri.to_string());
    for (name, value) in parts.headers.iter() {
        fastly_req.set_header(name.as_str(), value.as_bytes());
    }
    // Only Body::Once is supported. Body::Stream is intentionally not forwarded
    // because all outbound proxy bodies are built from Vec<u8> via EdgeBody::from()
    // and are therefore always Once. When this conversion moves to edgezero-adapter-fastly
    // it can use send_async_streaming() to handle Stream bodies properly.
    debug_assert!(
        matches!(&body, edgezero_core::body::Body::Once(_)),
        "unexpected Body::Stream in edge_request_to_fastly: body will be empty"
    );
    if let edgezero_core::body::Body::Once(bytes) = body {
        if !bytes.is_empty() {
            fastly_req.set_body(bytes.to_vec());
        }
    } else {
        log::warn!("edge_request_to_fastly: Body::Stream not supported; body will be empty");
    }
    fastly_req
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
        let fastly_req = edge_request_to_fastly(request.request);
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
        let fastly_req = edge_request_to_fastly(request.request);
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
        let mut saved_names: Vec<String> = Vec::with_capacity(pending_requests.len());

        for platform_req in pending_requests {
            let name = platform_req.backend_name().unwrap_or("").to_string();
            let inner = platform_req.downcast::<PendingRequest>().map_err(|_| {
                Report::new(PlatformError::HttpClient)
                    .attach("PlatformPendingRequest inner type is not fastly::PendingRequest")
            })?;
            fastly_pending.push(inner);
            saved_names.push(name);
        }

        let (result, remaining_fastly) = select(fastly_pending);

        // Re-attach saved backend names to the remaining pending requests.
        // Identify which request completed by matching the response backend name
        // to the saved names, then skip that index when rebuilding remaining.
        let completed_name = match &result {
            Ok(resp) => resp.get_backend_name().map(str::to_string),
            Err(_) => None,
        };
        let completed_idx = completed_name
            .as_deref()
            .and_then(|name| saved_names.iter().position(|n| n == name));
        if completed_name.is_some() && completed_idx.is_none() {
            log::warn!(
                "select: completed backend name not found in saved names; \
                 remaining requests will lose backend correlation"
            );
        }

        let remaining: Vec<PlatformPendingRequest> = if let Some(idx) = completed_idx {
            remaining_fastly
                .into_iter()
                .zip(
                    saved_names
                        .into_iter()
                        .enumerate()
                        .filter(|(i, _)| *i != idx)
                        .map(|(_, name)| name),
                )
                .map(|(req, name)| PlatformPendingRequest::new(req).with_backend_name(name))
                .collect()
        } else {
            remaining_fastly
                .into_iter()
                .map(PlatformPendingRequest::new)
                .collect()
        };

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

/// Fastly geo-lookup implementation of [`PlatformGeo`].
///
/// Uses [`geo_from_fastly`] from `trusted_server_core::geo` to avoid
/// duplicating the field-mapping logic present in `GeoInfo::from_request`.
pub struct FastlyPlatformGeo;

impl PlatformGeo for FastlyPlatformGeo {
    fn lookup(&self, client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(client_ip
            .and_then(geo_lookup)
            .map(|geo| geo_from_fastly(&geo)))
    }
}

// ---------------------------------------------------------------------------
// Entry-point helper
// ---------------------------------------------------------------------------

/// Construct a [`RuntimeServices`] instance from the incoming Fastly request.
///
/// Call this once at the entry point before dispatching to handlers.
/// `client_info` is populated from TLS and IP metadata available on the
/// request; geo lookup is deferred to handler time via
/// `services.geo().lookup(services.client_info().client_ip)`.
///
/// `kv_store` is an [`Arc<dyn PlatformKvStore>`] opened by the caller for
/// the primary KV store. Use [`open_kv_store`] to construct it.
#[must_use]
pub fn build_runtime_services(
    req: &Request,
    kv_store: Arc<dyn PlatformKvStore>,
) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(FastlyPlatformConfigStore))
        .secret_store(Arc::new(FastlyPlatformSecretStore))
        .kv_store(kv_store)
        .backend(Arc::new(FastlyPlatformBackend))
        .http_client(Arc::new(FastlyPlatformHttpClient))
        .geo(Arc::new(FastlyPlatformGeo))
        .client_info(ClientInfo {
            client_ip: req.get_client_ip_addr(),
            tls_protocol: req.get_tls_protocol().map(str::to_string),
            tls_cipher: req.get_tls_cipher_openssl_name().map(str::to_string),
        })
        .build()
}

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
    use std::sync::Arc;
    use std::time::Duration;

    use edgezero_core::body::Body;
    use edgezero_core::http::request_builder;
    use edgezero_core::key_value_store::NoopKvStore;

    use super::*;

    fn noop_kv_store() -> Arc<dyn PlatformKvStore> {
        Arc::new(NoopKvStore)
    }

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

    // --- ClientInfo extraction ----------------------------------------------

    #[test]
    fn build_runtime_services_client_info_is_none_without_tls() {
        let req = Request::get("https://example.com/");
        let services = build_runtime_services(&req, noop_kv_store());

        assert!(
            services.client_info().tls_protocol.is_none(),
            "should have no tls_protocol on plain test request"
        );
        assert!(
            services.client_info().tls_cipher.is_none(),
            "should have no tls_cipher on plain test request"
        );
    }

    #[test]
    fn build_runtime_services_returns_cloneable_services() {
        let req = Request::get("https://example.com/");
        let services = build_runtime_services(&req, noop_kv_store());
        let cloned = services.clone();

        assert_eq!(
            services.client_info().client_ip,
            cloned.client_info().client_ip,
            "should preserve client_ip through clone"
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
        let wrong = PlatformPendingRequest::new(42u32);
        let err = futures::executor::block_on(client.select(vec![wrong]))
            .expect_err("should return error for wrong inner type");

        assert!(
            matches!(err.current_context(), &PlatformError::HttpClient),
            "should be HttpClient error, got: {:?}",
            err.current_context()
        );
    }
}
