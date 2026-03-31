//! Fastly-backed implementations of the platform traits defined in
//! `trusted-server-core::platform`.
//!
//! This module also provides [`build_runtime_services`], a free function that
//! constructs a [`RuntimeServices`] instance once at the entry point from the
//! incoming Fastly request.

use core::fmt::Display;
use std::net::IpAddr;
use std::sync::Arc;

use edgezero_adapter_fastly::key_value_store::FastlyKvStore;
use edgezero_core::key_value_store::KvError;
use error_stack::{Report, ResultExt};
use fastly::geo::geo_lookup;
use fastly::{ConfigStore, Request, SecretStore};

use trusted_server_core::backend::BackendConfig;
use trusted_server_core::geo::geo_from_fastly;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore, PlatformError,
    PlatformGeo, PlatformHttpClient, PlatformHttpRequest, PlatformKvStore, PlatformPendingRequest,
    PlatformResponse, PlatformSecretStore, PlatformSelectResult, RuntimeServices, StoreId,
    StoreName,
};

pub(crate) use trusted_server_core::platform::UnavailableKvStore;

trait ConfigStoreReader: Sized {
    type LookupError: Display;

    fn try_get(&self, key: &str) -> Result<Option<String>, Self::LookupError>;
}

impl ConfigStoreReader for ConfigStore {
    type LookupError = fastly::config_store::LookupError;

    fn try_get(&self, key: &str) -> Result<Option<String>, Self::LookupError> {
        ConfigStore::try_get(self, key)
    }
}

fn get_config_value<S, Open, OpenError>(
    store_name: &str,
    key: &str,
    open_store: Open,
) -> Result<String, Report<PlatformError>>
where
    S: ConfigStoreReader,
    Open: FnOnce() -> Result<S, OpenError>,
    OpenError: Display,
{
    let store = open_store().map_err(|error| {
        Report::new(PlatformError::ConfigStore).attach(format!(
            "failed to open config store '{store_name}': {error}"
        ))
    })?;

    store
        .try_get(key)
        .map_err(|error| {
            Report::new(PlatformError::ConfigStore).attach(format!(
                "lookup for key '{key}' in config store '{store_name}' failed: {error}"
            ))
        })?
        .ok_or_else(|| {
            Report::new(PlatformError::ConfigStore).attach(format!(
                "key '{key}' not found in config store '{store_name}'"
            ))
        })
}

enum SecretReadError<LookupError, DecryptError> {
    Lookup(LookupError),
    Decrypt(DecryptError),
}

type SecretBytesResult<LookupError, DecryptError> =
    Result<Option<Vec<u8>>, SecretReadError<LookupError, DecryptError>>;

trait SecretStoreReader: Sized {
    type LookupError: Display;
    type DecryptError: Display;

    fn try_get_bytes(&self, key: &str) -> SecretBytesResult<Self::LookupError, Self::DecryptError>;
}

impl SecretStoreReader for SecretStore {
    type LookupError = fastly::secret_store::LookupError;
    type DecryptError = fastly::secret_store::DecryptError;

    fn try_get_bytes(&self, key: &str) -> SecretBytesResult<Self::LookupError, Self::DecryptError> {
        let secret = self.try_get(key).map_err(SecretReadError::Lookup)?;
        let Some(secret) = secret else {
            return Ok(None);
        };

        secret
            .try_plaintext()
            .map(|bytes| Some(bytes.into_iter().collect()))
            .map_err(SecretReadError::Decrypt)
    }
}

fn get_secret_bytes<S, Open, OpenError>(
    store_name: &str,
    key: &str,
    open_store: Open,
) -> Result<Vec<u8>, Report<PlatformError>>
where
    S: SecretStoreReader,
    Open: FnOnce() -> Result<S, OpenError>,
    OpenError: Display,
{
    let store = open_store().map_err(|error| {
        Report::new(PlatformError::SecretStore).attach(format!(
            "failed to open secret store '{store_name}': {error}"
        ))
    })?;

    store
        .try_get_bytes(key)
        .map_err(|error| match error {
            SecretReadError::Lookup(error) => Report::new(PlatformError::SecretStore).attach(
                format!("lookup for key '{key}' in secret store '{store_name}' failed: {error}"),
            ),
            SecretReadError::Decrypt(error) => Report::new(PlatformError::SecretStore)
                .attach(format!("failed to decrypt secret '{key}': {error}")),
        })?
        .ok_or_else(|| {
            Report::new(PlatformError::SecretStore).attach(format!(
                "key '{key}' not found in secret store '{store_name}'"
            ))
        })
}

// ---------------------------------------------------------------------------
// FastlyPlatformConfigStore
// ---------------------------------------------------------------------------

/// Fastly [`ConfigStore`]-backed implementation of [`PlatformConfigStore`].
///
/// Stateless — the store name is supplied per call, matching the trait
/// signature. This replaces the store-name-at-construction pattern of
/// [`trusted_server_core::storage::FastlyConfigStore`].
///
/// Write methods (`put`, `delete`) are not yet implemented and return
/// [`PlatformError::NotImplemented`]. Management writes land in a follow-up PR.
pub struct FastlyPlatformConfigStore;

impl PlatformConfigStore for FastlyPlatformConfigStore {
    fn get(&self, store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        let name = store_name.as_ref();
        get_config_value::<ConfigStore, _, _>(name, key, || ConfigStore::try_open(name))
    }

    fn put(
        &self,
        _store_id: &StoreId,
        _key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::NotImplemented))
    }

    fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::NotImplemented))
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
/// Write methods (`create`, `delete`) are not yet implemented and return
/// [`PlatformError::NotImplemented`]. Management writes land in a follow-up PR.
pub struct FastlyPlatformSecretStore;

impl PlatformSecretStore for FastlyPlatformSecretStore {
    fn get_bytes(
        &self,
        store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        let name = store_name.as_ref();
        get_secret_bytes::<SecretStore, _, _>(name, key, || SecretStore::open(name))
    }

    fn create(
        &self,
        _store_id: &StoreId,
        _name: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::NotImplemented))
    }

    fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::NotImplemented))
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
/// `services.geo.lookup(services.client_info.client_ip)`.
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

    struct StubConfigStore {
        value: Result<Option<String>, &'static str>,
    }

    impl ConfigStoreReader for StubConfigStore {
        type LookupError = &'static str;

        fn try_get(&self, _key: &str) -> Result<Option<String>, Self::LookupError> {
            self.value.clone()
        }
    }

    enum StubSecretReadError {
        Decrypt(&'static str),
    }

    struct StubSecretStore {
        value: Result<Option<Vec<u8>>, StubSecretReadError>,
    }

    impl SecretStoreReader for StubSecretStore {
        type LookupError = &'static str;
        type DecryptError = &'static str;

        fn try_get_bytes(
            &self,
            _key: &str,
        ) -> SecretBytesResult<Self::LookupError, Self::DecryptError> {
            match &self.value {
                Ok(Some(bytes)) => Ok(Some(bytes.clone())),
                Ok(None) => Ok(None),
                Err(StubSecretReadError::Decrypt(error)) => Err(SecretReadError::Decrypt(*error)),
            }
        }
    }

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
            services.client_info.tls_protocol.is_none(),
            "should have no tls_protocol on plain test request"
        );
        assert!(
            services.client_info.tls_cipher.is_none(),
            "should have no tls_cipher on plain test request"
        );
    }

    #[test]
    fn build_runtime_services_returns_cloneable_services() {
        let req = Request::get("https://example.com/");
        let services = build_runtime_services(&req, noop_kv_store());
        let cloned = services.clone();

        assert_eq!(
            services.client_info.client_ip, cloned.client_info.client_ip,
            "should preserve client_ip through clone"
        );
    }

    #[test]
    fn get_config_value_returns_error_when_lookup_fails() {
        let err = get_config_value::<StubConfigStore, _, _>("jwks_store", "active-kids", || {
            Ok::<StubConfigStore, &'static str>(StubConfigStore {
                value: Err("lookup failed"),
            })
        })
        .expect_err("should return an error when config lookup fails");

        assert!(
            matches!(err.current_context(), &PlatformError::ConfigStore),
            "should surface as PlatformError::ConfigStore"
        );
    }

    #[test]
    fn get_secret_bytes_returns_error_when_decrypt_fails() {
        let err = get_secret_bytes::<StubSecretStore, _, _>("signing_keys", "kid", || {
            Ok::<StubSecretStore, &'static str>(StubSecretStore {
                value: Err(StubSecretReadError::Decrypt("decrypt failed")),
            })
        })
        .expect_err("should return an error when secret decryption fails");

        assert!(
            matches!(err.current_context(), &PlatformError::SecretStore),
            "should surface as PlatformError::SecretStore"
        );
    }

    #[test]
    fn get_secret_bytes_returns_error_when_open_fails() {
        let err = get_secret_bytes::<StubSecretStore, _, _>("signing_keys", "active", || {
            Err::<StubSecretStore, &'static str>("permission denied")
        })
        .expect_err("should return an error when the secret store cannot be opened");

        assert!(
            matches!(err.current_context(), &PlatformError::SecretStore),
            "should surface as PlatformError::SecretStore"
        );
    }

    // --- FastlyPlatformSecretStore write stubs ------------------------------

    #[test]
    fn fastly_platform_secret_store_create_returns_not_implemented() {
        let store = FastlyPlatformSecretStore;
        let err = store
            .create(&StoreId::from("test-store-id"), "my-secret", "value")
            .expect_err("should return an error for unimplemented create");

        assert!(
            matches!(err.current_context(), &PlatformError::NotImplemented),
            "should report NotImplemented while secret store write is not yet implemented"
        );
    }

    #[test]
    fn fastly_platform_secret_store_delete_returns_not_implemented() {
        let store = FastlyPlatformSecretStore;
        let err = store
            .delete(&StoreId::from("test-store-id"), "my-secret")
            .expect_err("should return an error for unimplemented delete");

        assert!(
            matches!(err.current_context(), &PlatformError::NotImplemented),
            "should report NotImplemented while secret store write is not yet implemented"
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
