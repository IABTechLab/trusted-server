use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use error_stack::{Report, ResultExt as _};
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore, PlatformError,
    PlatformGeo, PlatformHttpClient, PlatformHttpRequest, PlatformPendingRequest, PlatformResponse,
    PlatformSecretStore, PlatformSelectResult, RuntimeServices, StoreId, StoreName,
};

// ---------------------------------------------------------------------------
// Env-var naming helpers
// ---------------------------------------------------------------------------

/// Normalize a store name or key for use as an environment-variable segment.
///
/// Uppercases and replaces hyphens, dots, and spaces with underscores.
fn normalize_env_segment(s: &str) -> String {
    s.to_uppercase().replace(['-', '.', ' '], "_")
}

fn config_env_var(store_name: &str, key: &str) -> String {
    format!(
        "TRUSTED_SERVER_CONFIG_{}_{}",
        normalize_env_segment(store_name),
        normalize_env_segment(key),
    )
}

fn secret_env_var(store_name: &str, key: &str) -> String {
    format!(
        "TRUSTED_SERVER_SECRET_{}_{}",
        normalize_env_segment(store_name),
        normalize_env_segment(key),
    )
}

// ---------------------------------------------------------------------------
// PlatformConfigStore
// ---------------------------------------------------------------------------

/// Environment-variable–backed config store for the Axum dev server.
///
/// Reads from `TRUSTED_SERVER_CONFIG_{STORE}_{KEY}` (uppercased, hyphens→underscores).
/// Write operations are unsupported in local development.
pub struct AxumPlatformConfigStore;

impl PlatformConfigStore for AxumPlatformConfigStore {
    fn get(&self, store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        let var_name = config_env_var(store_name.as_ref(), key);
        std::env::var(&var_name).map_err(|_| {
            Report::new(PlatformError::ConfigStore).attach(format!(
                "env var '{var_name}' not set — export it to supply this config value"
            ))
        })
    }

    fn put(
        &self,
        store_id: &StoreId,
        key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        log::warn!(
            "AxumPlatformConfigStore: write to store '{}' key '{}' ignored \
             (config store writes are not supported on the Axum dev server)",
            store_id.as_ref(),
            key
        );
        Err(Report::new(PlatformError::ConfigStore)
            .attach("config store writes are not supported on the Axum dev server"))
    }

    fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
        log::warn!(
            "AxumPlatformConfigStore: delete from store '{}' key '{}' ignored \
             (config store deletes are not supported on the Axum dev server)",
            store_id.as_ref(),
            key
        );
        Err(Report::new(PlatformError::ConfigStore)
            .attach("config store deletes are not supported on the Axum dev server"))
    }
}

// ---------------------------------------------------------------------------
// PlatformSecretStore
// ---------------------------------------------------------------------------

/// Environment-variable–backed secret store for the Axum dev server.
///
/// Reads from `TRUSTED_SERVER_SECRET_{STORE}_{KEY}` as raw UTF-8 bytes.
/// Write operations are unsupported in local development.
pub struct AxumPlatformSecretStore;

impl PlatformSecretStore for AxumPlatformSecretStore {
    fn get_bytes(
        &self,
        store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        let var_name = secret_env_var(store_name.as_ref(), key);
        std::env::var(&var_name)
            .map(String::into_bytes)
            .map_err(|_| {
                Report::new(PlatformError::SecretStore).attach(format!(
                    "env var '{var_name}' not set — export it to supply this secret value"
                ))
            })
    }

    fn create(
        &self,
        store_id: &StoreId,
        name: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        log::warn!(
            "AxumPlatformSecretStore: create '{}' in store '{}' ignored \
             (secret store writes are not supported on the Axum dev server)",
            name,
            store_id.as_ref()
        );
        Err(Report::new(PlatformError::SecretStore)
            .attach("secret store writes are not supported on the Axum dev server"))
    }

    fn delete(&self, store_id: &StoreId, name: &str) -> Result<(), Report<PlatformError>> {
        log::warn!(
            "AxumPlatformSecretStore: delete '{}' from store '{}' ignored \
             (secret store deletes are not supported on the Axum dev server)",
            name,
            store_id.as_ref()
        );
        Err(Report::new(PlatformError::SecretStore)
            .attach("secret store deletes are not supported on the Axum dev server"))
    }
}

// ---------------------------------------------------------------------------
// PlatformBackend
// ---------------------------------------------------------------------------

/// No-op backend for the Axum dev server.
///
/// Returns a deterministic name; `ensure` is a no-op returning the same name.
/// The Axum HTTP client sends directly to URIs and ignores backend names.
pub struct AxumPlatformBackend;

impl PlatformBackend for AxumPlatformBackend {
    fn predict_name(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        let port = spec
            .port
            .unwrap_or(if spec.scheme == "https" { 443 } else { 80 });
        Ok(format!(
            "{}_{}_{}",
            normalize_env_segment(&spec.scheme),
            normalize_env_segment(&spec.host),
            port,
        ))
    }

    fn ensure(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        self.predict_name(spec)
    }
}

// ---------------------------------------------------------------------------
// PlatformGeo
// ---------------------------------------------------------------------------

/// No-op geo implementation — geographic lookup is unavailable in local development.
pub struct AxumPlatformGeo;

impl PlatformGeo for AxumPlatformGeo {
    fn lookup(&self, _client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// PlatformHttpClient
// ---------------------------------------------------------------------------

/// Raw response parts carried through `send_async` → `select`.
///
/// Stores `(status, headers, body)` instead of `PlatformResponse` because
/// `Body::Stream` is `!Send`, making it incompatible with `Box<dyn Any + Send + Sync>`
/// required by [`PlatformPendingRequest`]. Same pattern as `StubPendingResponse`
/// in `trusted-server-core::platform::test_support`.
struct AxumPendingResponse {
    backend_name: String,
    status: u16,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
}

/// reqwest-backed HTTP client for the Axum dev server.
///
/// `send_async` + `select` use eager sequential evaluation acceptable for local
/// development. True async fan-out is not needed at dev-server scale.
pub struct AxumPlatformHttpClient {
    client: reqwest::Client,
}

impl AxumPlatformHttpClient {
    /// Create a new client with sensible dev-server timeouts.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `reqwest::Client` cannot be built (should not
    /// happen with the default TLS configuration on any supported platform).
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(30))
                .build()
                .expect("should build reqwest client"),
        }
    }

    async fn execute(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        let uri = request.request.uri().to_string();
        let method = reqwest::Method::from_bytes(request.request.method().as_str().as_bytes())
            .change_context(PlatformError::HttpClient)?;

        let mut builder = self.client.request(method, &uri);
        for (name, value) in request.request.headers() {
            builder = builder.header(name.as_str(), value.as_bytes());
        }

        let (_, body) = request.request.into_parts();
        match body {
            edgezero_core::body::Body::Once(bytes) => {
                if !bytes.is_empty() {
                    builder = builder.body(bytes);
                }
            }
            edgezero_core::body::Body::Stream(_) => {
                log::warn!(
                    "AxumPlatformHttpClient: Body::Stream is not supported; \
                     outbound request body will be empty"
                );
            }
        }

        let resp = builder
            .send()
            .await
            .change_context(PlatformError::HttpClient)
            .attach(format!("outbound request to {uri} failed"))?;

        let status = resp.status().as_u16();
        let mut edge_builder = edgezero_core::http::response_builder().status(status);
        for (name, value) in resp.headers() {
            edge_builder = edge_builder.header(name.as_str(), value.as_bytes());
        }
        let resp_bytes = resp
            .bytes()
            .await
            .change_context(PlatformError::HttpClient)?;
        let edge_resp = edge_builder
            .body(edgezero_core::body::Body::from(resp_bytes.to_vec()))
            .change_context(PlatformError::HttpClient)?;

        Ok(PlatformResponse::new(edge_resp).with_backend_name(request.backend_name))
    }
}

impl Default for AxumPlatformHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait(?Send)]
impl PlatformHttpClient for AxumPlatformHttpClient {
    async fn send(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        self.execute(request).await
    }

    async fn send_async(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
        // Dev-server divergence: execution is eager — errors surface here, not at
        // select() time. On Fastly the request is in-flight until select() resolves it.
        log::debug!(
            "AxumPlatformHttpClient::send_async: executing eagerly (Fastly surfaces errors at select)"
        );
        let backend_name = request.backend_name.clone();
        let response = self.execute(request).await?;

        let status = response.response.status().as_u16();
        let headers: Vec<(String, Vec<u8>)> = response
            .response
            .headers()
            .iter()
            .map(|(n, v)| (n.to_string(), v.as_bytes().to_vec()))
            .collect();
        let body_bytes = match response.response.into_body() {
            edgezero_core::body::Body::Once(bytes) => bytes.to_vec(),
            edgezero_core::body::Body::Stream(_) => vec![],
        };

        let pending = AxumPendingResponse {
            backend_name: backend_name.clone(),
            status,
            headers,
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
                .attach("select called with an empty pending_requests list"));
        }

        // Dev-server divergence: pops index 0 unconditionally — not "first to complete".
        // Safe here because send_async already ran eagerly, but any test verifying
        // parallel fan-out ordering against the Fastly runtime should use a real
        // Fastly environment.
        log::debug!(
            "AxumPlatformHttpClient::select: returning index 0 (sequential, not parallel fan-out)"
        );
        let ready_platform = pending_requests.remove(0);
        let pending = ready_platform
            .downcast::<AxumPendingResponse>()
            .map_err(|_| {
                Report::new(PlatformError::HttpClient)
                    .attach("unexpected inner type in AxumPlatformHttpClient::select")
            })?;

        let mut builder = edgezero_core::http::response_builder().status(pending.status);
        for (name, value) in &pending.headers {
            builder = builder.header(name.as_str(), value.as_slice());
        }
        let edge_resp = builder
            .body(edgezero_core::body::Body::from(pending.body))
            .change_context(PlatformError::HttpClient)?;

        let ready = Ok(PlatformResponse::new(edge_resp).with_backend_name(pending.backend_name));
        Ok(PlatformSelectResult {
            ready,
            remaining: pending_requests,
        })
    }
}

// ---------------------------------------------------------------------------
// build_runtime_services
// ---------------------------------------------------------------------------

/// Construct [`RuntimeServices`] for an incoming Axum request.
///
/// # Degraded features in dev
///
/// KV store is [`trusted_server_core::platform::UnavailableKvStore`] — any route
/// touching synthetic-ID or consent KV will degrade gracefully. A `warn` log is
/// emitted once per process.
pub fn build_runtime_services(
    ctx: &edgezero_core::context::RequestContext,
    http_client: Arc<AxumPlatformHttpClient>,
) -> RuntimeServices {
    static KV_WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    KV_WARNED.get_or_init(|| {
        log::warn!(
            "Axum dev server: KV store is unavailable (UnavailableKvStore). \
             Routes that depend on synthetic-ID or consent KV will degrade gracefully."
        );
    });

    let client_ip = edgezero_adapter_axum::AxumRequestContext::get(ctx.request())
        .and_then(|c| c.remote_addr)
        .map(|addr| addr.ip());

    RuntimeServices::builder()
        .config_store(Arc::new(AxumPlatformConfigStore))
        .secret_store(Arc::new(AxumPlatformSecretStore))
        .kv_store(Arc::new(trusted_server_core::platform::UnavailableKvStore))
        .backend(Arc::new(AxumPlatformBackend))
        .http_client(http_client)
        .geo(Arc::new(AxumPlatformGeo))
        .client_info(ClientInfo {
            client_ip,
            tls_protocol: None,
            tls_cipher: None,
        })
        .build()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn config_store_reads_from_env_var() {
        temp_env::with_var(
            "TRUSTED_SERVER_CONFIG_MY_STORE_MY_KEY",
            Some("test-value"),
            || {
                let store = AxumPlatformConfigStore;
                let result = store
                    .get(&StoreName::from("my-store"), "my-key")
                    .expect("should read env var");
                assert_eq!(result, "test-value", "should return env var value");
            },
        );
    }

    #[test]
    fn config_store_returns_error_for_missing_env_var() {
        let store = AxumPlatformConfigStore;
        let result = store.get(
            &StoreName::from("nonexistent-store-zzz"),
            "nonexistent-key-zzz",
        );
        assert!(result.is_err(), "should error for missing env var");
    }

    #[test]
    fn secret_store_reads_bytes_from_env_var() {
        temp_env::with_var(
            "TRUSTED_SERVER_SECRET_MY_SECRETS_MY_SECRET",
            Some("hello"),
            || {
                let store = AxumPlatformSecretStore;
                let result = store
                    .get_bytes(&StoreName::from("my-secrets"), "my-secret")
                    .expect("should read env var as bytes");
                assert_eq!(result, b"hello", "should return raw bytes");
            },
        );
    }

    #[test]
    fn backend_predict_name_returns_deterministic_string() {
        let backend = AxumPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "example.com".to_string(),
            port: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_secs(15),
        };
        let name1 = backend.predict_name(&spec).expect("should return a name");
        let name2 = backend
            .predict_name(&spec)
            .expect("should return same name");
        assert!(!name1.is_empty(), "should return a non-empty name");
        assert_eq!(name1, name2, "should be deterministic");
    }

    #[test]
    fn backend_ensure_returns_same_name_as_predict() {
        let backend = AxumPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "example.com".to_string(),
            port: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_secs(15),
        };
        assert_eq!(
            backend.predict_name(&spec).expect("should return name"),
            backend.ensure(&spec).expect("should return name"),
            "ensure should equal predict_name"
        );
    }

    #[test]
    fn geo_always_returns_none() {
        let geo = AxumPlatformGeo;
        let no_ip = geo.lookup(None).expect("should not error");
        assert!(no_ip.is_none(), "should return None for no IP");
        let with_ip = geo
            .lookup(Some("127.0.0.1".parse().expect("should parse IP")))
            .expect("should not error");
        assert!(with_ip.is_none(), "should return None for any IP");
    }
}
