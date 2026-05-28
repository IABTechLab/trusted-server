use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use edgezero_core::http::{HeaderMap, HeaderName, HeaderValue, header};
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

type SpawnedRequestResult = Result<(u16, Vec<(String, Vec<u8>)>, Vec<u8>), Report<PlatformError>>;

fn sanitized_response_headers(headers: &HeaderMap) -> Vec<(String, Vec<u8>)> {
    let connection_tokens = connection_header_tokens(headers);

    headers
        .iter()
        .filter(|(name, _)| !is_hop_by_hop_response_header(name, &connection_tokens))
        .map(|(name, value)| (name.to_string(), value.as_bytes().to_vec()))
        .collect()
}

fn connection_header_tokens(headers: &HeaderMap) -> Vec<HeaderName> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(header_value_to_str)
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .filter_map(|token| HeaderName::from_bytes(token.as_bytes()).ok())
        .collect()
}

fn header_value_to_str(value: &HeaderValue) -> Option<&str> {
    value.to_str().ok()
}

fn is_hop_by_hop_response_header(name: &HeaderName, connection_tokens: &[HeaderName]) -> bool {
    name == header::CONNECTION
        || name == header::PROXY_AUTHENTICATE
        || name == header::PROXY_AUTHORIZATION
        || name == header::TE
        || name == header::TRAILER
        || name == header::TRANSFER_ENCODING
        || name == header::UPGRADE
        || name.as_str().eq_ignore_ascii_case("keep-alive")
        || connection_tokens.iter().any(|token| token == name)
}

/// Buffered response parts from a spawned outbound request.
///
/// Stored inside [`PlatformPendingRequest`] so that [`AxumPlatformHttpClient::select`]
/// can poll multiple in-flight handles concurrently via
/// [`futures::future::select_all`].
struct AxumPendingHandle {
    backend_name: String,
    handle: tokio::task::JoinHandle<SpawnedRequestResult>,
}

/// reqwest-backed HTTP client for the Axum dev server.
///
/// `send_async` buffers any `Body::Stream` in the calling context, then spawns
/// a `tokio` task for each outbound request so that multiple `send_async` calls
/// run concurrently. `select` uses [`futures::future::select_all`] to wait for
/// the first completing handle, preserving fan-out semantics.
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
                // Disable automatic redirects: core proxy code enforces redirect
                // limits and allowed_domains checks itself. Without this, reqwest
                // would follow Location headers internally and bypass those checks.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("should build reqwest client"),
        }
    }

    /// Drain `body` to a `Vec<u8>`.
    ///
    /// For `Body::Stream` this awaits every chunk in the current async context
    /// (where `LocalBoxStream` is valid) before the bytes are moved into a
    /// `tokio::spawn` task that requires `Send`.
    async fn buffer_body(
        body: edgezero_core::body::Body,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        match body {
            edgezero_core::body::Body::Once(bytes) => Ok(bytes.to_vec()),
            edgezero_core::body::Body::Stream(mut stream) => {
                log::debug!("buffering Body::Stream into Vec<u8> for outbound request");
                use futures::StreamExt as _;
                let mut buf = Vec::new();
                while let Some(chunk) = stream.next().await {
                    let bytes = chunk.map_err(|e| {
                        Report::new(PlatformError::HttpClient)
                            .attach(format!("failed to buffer outbound streaming body: {e}"))
                    })?;
                    buf.extend_from_slice(&bytes);
                }
                Ok(buf)
            }
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
        let body_bytes = Self::buffer_body(body).await?;
        if !body_bytes.is_empty() {
            builder = builder.body(body_bytes);
        }

        let resp = builder
            .send()
            .await
            .change_context(PlatformError::HttpClient)
            .attach(format!("outbound request to {uri} failed"))?;

        let status = resp.status().as_u16();
        let mut edge_builder = edgezero_core::http::response_builder().status(status);
        for (name, value) in sanitized_response_headers(resp.headers()) {
            edge_builder = edge_builder.header(name.as_str(), value.as_slice());
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
        let backend_name = request.backend_name.clone();

        // Extract all Send-compatible parts before spawning.
        let uri = request.request.uri().to_string();
        let method_bytes = request.request.method().as_str().as_bytes().to_vec();
        let headers: Vec<(String, Vec<u8>)> = request
            .request
            .headers()
            .iter()
            .map(|(n, v)| (n.to_string(), v.as_bytes().to_vec()))
            .collect();

        // Buffer any LocalBoxStream body here in the ?Send context before spawn.
        let (_, body) = request.request.into_parts();
        let body_bytes = Self::buffer_body(body).await?;

        let client = self.client.clone();
        let handle = tokio::spawn(async move {
            let method = reqwest::Method::from_bytes(&method_bytes)
                .map_err(|e| Report::new(PlatformError::HttpClient).attach(e.to_string()))?;
            let mut builder = client.request(method, &uri);
            for (name, value) in &headers {
                builder = builder.header(name.as_str(), value.as_slice());
            }
            if !body_bytes.is_empty() {
                builder = builder.body(body_bytes);
            }
            let resp = builder.send().await.map_err(|e| {
                Report::new(PlatformError::HttpClient)
                    .attach(format!("outbound request to {uri} failed: {e}"))
            })?;
            let status = resp.status().as_u16();
            let resp_headers = sanitized_response_headers(resp.headers());
            let body = resp
                .bytes()
                .await
                .map_err(|e| Report::new(PlatformError::HttpClient).attach(e.to_string()))?
                .to_vec();
            Ok::<_, Report<PlatformError>>((status, resp_headers, body))
        });

        let pending = AxumPendingHandle {
            backend_name: backend_name.clone(),
            handle,
        };
        Ok(PlatformPendingRequest::new(pending).with_backend_name(backend_name))
    }

    async fn select(
        &self,
        pending_requests: Vec<PlatformPendingRequest>,
    ) -> Result<PlatformSelectResult, Report<PlatformError>> {
        if pending_requests.is_empty() {
            return Err(Report::new(PlatformError::HttpClient)
                .attach("select called with an empty pending_requests list"));
        }

        let mut handles: Vec<AxumPendingHandle> = pending_requests
            .into_iter()
            .map(|pr| {
                pr.downcast::<AxumPendingHandle>().map_err(|_| {
                    Report::new(PlatformError::HttpClient)
                        .attach("unexpected inner type in AxumPlatformHttpClient::select")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let backend_names: Vec<String> = handles.iter().map(|h| h.backend_name.clone()).collect();
        let join_handles: Vec<_> = handles.drain(..).map(|h| h.handle).collect();

        let (result, ready_idx, remaining_handles) =
            futures::future::select_all(join_handles).await;

        let remaining: Vec<PlatformPendingRequest> = remaining_handles
            .into_iter()
            .enumerate()
            .map(|(i, handle)| {
                let original_idx = if i < ready_idx { i } else { i + 1 };
                let bn = backend_names[original_idx].clone();
                PlatformPendingRequest::new(AxumPendingHandle {
                    backend_name: bn.clone(),
                    handle,
                })
                .with_backend_name(bn)
            })
            .collect();

        let backend_name = backend_names[ready_idx].clone();

        // Map join panics and per-request errors into ready: Err(...) so that the
        // auction orchestrator can log the failure and continue with remaining providers
        // rather than treating one bad provider as a fatal select() failure.
        let ready = result
            .map_err(|e| {
                Report::new(PlatformError::HttpClient)
                    .attach(format!("auction request task panicked: {e}"))
            })
            .and_then(|inner| inner)
            .and_then(|(status, headers, body)| {
                let mut builder = edgezero_core::http::response_builder().status(status);
                for (name, value) in &headers {
                    builder = builder.header(name.as_str(), value.as_slice());
                }
                builder
                    .body(edgezero_core::body::Body::from(body))
                    .change_context(PlatformError::HttpClient)
            })
            .map(|edge_resp| PlatformResponse::new(edge_resp).with_backend_name(backend_name));

        Ok(PlatformSelectResult { ready, remaining })
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
pub fn build_runtime_services(ctx: &edgezero_core::context::RequestContext) -> RuntimeServices {
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

    use trusted_server_core::platform::{
        PlatformBackend, PlatformConfigStore, PlatformGeo, PlatformKvStore, PlatformSecretStore,
    };

    // Stateless shims are promoted to process-wide statics so callers clone
    // an existing Arc instead of allocating a new one per request.
    static CONFIG_STORE: std::sync::OnceLock<Arc<dyn PlatformConfigStore>> =
        std::sync::OnceLock::new();
    static SECRET_STORE: std::sync::OnceLock<Arc<dyn PlatformSecretStore>> =
        std::sync::OnceLock::new();
    static KV_STORE: std::sync::OnceLock<Arc<dyn PlatformKvStore>> = std::sync::OnceLock::new();
    static BACKEND: std::sync::OnceLock<Arc<dyn PlatformBackend>> = std::sync::OnceLock::new();
    static GEO: std::sync::OnceLock<Arc<dyn PlatformGeo>> = std::sync::OnceLock::new();

    RuntimeServices::builder()
        .config_store(Arc::clone(CONFIG_STORE.get_or_init(|| {
            Arc::new(AxumPlatformConfigStore) as Arc<dyn PlatformConfigStore>
        })))
        .secret_store(Arc::clone(SECRET_STORE.get_or_init(|| {
            Arc::new(AxumPlatformSecretStore) as Arc<dyn PlatformSecretStore>
        })))
        .kv_store(Arc::clone(KV_STORE.get_or_init(|| {
            Arc::new(trusted_server_core::platform::UnavailableKvStore) as Arc<dyn PlatformKvStore>
        })))
        .backend(Arc::clone(BACKEND.get_or_init(|| {
            Arc::new(AxumPlatformBackend) as Arc<dyn PlatformBackend>
        })))
        // Keep the HTTP client request-scoped in the dev adapter. Sharing a pooled
        // client across requests previously regressed the Next.js server-action →
        // API-route integration flow by reusing a poisoned connection after a
        // truncated POST. Revisit pooling if profiling shows allocation cost.
        .http_client(Arc::new(AxumPlatformHttpClient::new()))
        .geo(Arc::clone(GEO.get_or_init(|| {
            Arc::new(AxumPlatformGeo) as Arc<dyn PlatformGeo>
        })))
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
    use edgezero_core::body::Body as EdgeBody;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_client_strips_hop_by_hop_response_headers() {
        let url = serve_raw_response(
            b"HTTP/1.1 200 OK\r\n\
              Transfer-Encoding: chunked\r\n\
              Connection: keep-alive, x-remove-me\r\n\
              Keep-Alive: timeout=5\r\n\
              X-Remove-Me: listed-by-connection\r\n\
              X-Preserve-Me: application-header\r\n\
              \r\n\
              2\r\n\
              ok\r\n\
              0\r\n\
              \r\n",
        )
        .await;

        let request = edgezero_core::http::request_builder()
            .uri(url)
            .body(EdgeBody::empty())
            .expect("should build outbound request");

        let response = AxumPlatformHttpClient::new()
            .send(PlatformHttpRequest::new(request, "test_backend"))
            .await
            .expect("should proxy raw response")
            .response;

        assert!(
            response.headers().get(header::TRANSFER_ENCODING).is_none(),
            "should strip transfer-encoding"
        );
        assert!(
            response.headers().get(header::CONNECTION).is_none(),
            "should strip connection"
        );
        assert!(
            response.headers().get("keep-alive").is_none(),
            "should strip keep-alive"
        );
        assert!(
            response.headers().get("x-remove-me").is_none(),
            "should strip headers named by connection"
        );
        assert_eq!(
            response
                .headers()
                .get("x-preserve-me")
                .and_then(|value| value.to_str().ok()),
            Some("application-header"),
            "should preserve end-to-end headers"
        );
        assert_eq!(
            response.into_body().into_bytes().as_ref(),
            b"ok",
            "should preserve decoded response body"
        );
    }

    async fn serve_raw_response(response: &'static [u8]) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("should bind raw HTTP test server");
        let addr = listener.local_addr().expect("should read local address");

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("should accept request");
            let mut request = [0; 1024];
            let _ = stream
                .read(&mut request)
                .await
                .expect("should read request");
            stream
                .write_all(response)
                .await
                .expect("should write response");
        });

        format!("http://{addr}/")
    }
}
