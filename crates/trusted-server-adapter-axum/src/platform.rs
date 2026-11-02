use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use edgezero_core::http::{HeaderMap, HeaderName, HeaderValue, header};
use error_stack::{Report, ResultExt as _};
use trusted_server_core::platform::{
    BackendNamingPolicy, ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec,
    PlatformConfigStore, PlatformError, PlatformGeo, PlatformHttpClient, PlatformHttpRequest,
    PlatformPendingRequest, PlatformResponse, PlatformSecretStore, PlatformSelectResult,
    RuntimeServices, StoreId, StoreName,
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

/// Returns the environment-variable name for a config store entry.
#[must_use]
pub fn config_env_var(store_name: &str, key: &str) -> String {
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
    fn naming_policy(&self) -> BackendNamingPolicy {
        BackendNamingPolicy::Axum
    }

    fn predict_name(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        self.naming_policy()
            .predict(spec)
            .map(|prediction| prediction.name)
            .change_context(PlatformError::Backend)
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

impl Drop for AxumPendingHandle {
    fn drop(&mut self) {
        // Abort instead of detaching: when the orchestrator hits the auction
        // deadline and drops the remaining pending requests, the abandoned
        // bidder tasks would otherwise keep running for up to the 30s
        // transport timeout.
        self.handle.abort();
    }
}

/// Resolves to the backend name together with the task result so that
/// [`futures::future::select_all`] callers never have to reconstruct which
/// backend a completion belongs to by position. `select_all` removes the
/// ready future with `swap_remove` and makes no ordering guarantee for the
/// remaining futures, so positional bookkeeping would mislabel them.
impl Future for AxumPendingHandle {
    type Output = (String, Result<SpawnedRequestResult, tokio::task::JoinError>);

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.handle).poll(cx) {
            Poll::Ready(result) => {
                let backend_name = std::mem::take(&mut self.backend_name);
                Poll::Ready((backend_name, result))
            }
            Poll::Pending => Poll::Pending,
        }
    }
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
        // Upstream responses are buffered whole with no cap — acceptable for
        // the dev server, but log the size so a large or hostile upstream is
        // visible instead of silently growing the heap.
        log::debug!(
            "buffered {} upstream response bytes from {uri}",
            resp_bytes.len()
        );
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
            // Same unbounded-buffering note as the synchronous path: log the
            // size so large upstream responses are visible in dev.
            log::debug!("buffered {} upstream response bytes from {uri}", body.len());
            Ok::<_, Report<PlatformError>>((status, resp_headers, body))
        });

        let pending = AxumPendingHandle {
            backend_name: backend_name.clone(),
            handle,
        };
        Ok(PlatformPendingRequest::new(pending).with_backend_name(backend_name))
    }

    async fn select_until(
        &self,
        pending_requests: Vec<PlatformPendingRequest>,
        deadline: web_time::Instant,
    ) -> Result<Option<PlatformSelectResult>, Report<PlatformError>> {
        let mut handles = recover_pending_handles(pending_requests)?;
        if handles.is_empty() {
            return Err(Report::new(PlatformError::HttpClient)
                .attach("select_until called with no pending requests"));
        }
        let remaining = deadline.saturating_duration_since(web_time::Instant::now());
        if !remaining.is_zero() {
            // Borrow the handles so timer expiry cannot drop completed siblings
            // before the final nonblocking snapshot inspects them.
            let selected =
                tokio::time::timeout(remaining, futures::future::select_all(handles.iter_mut()))
                    .await;
            if let Ok(((backend_name, result), index, _)) = selected {
                drop(handles.swap_remove(index));
                return Ok(Some(completed_selection(backend_name, result, handles)));
            }
        }
        // Snapshot once: only completed tasks survive into subsequent calls.
        // An unfinished task is aborted on drop and cannot become a late bid.
        handles.retain(|pending| pending.handle.is_finished());
        if handles.is_empty() {
            return Ok(None);
        }
        let ((backend_name, result), _, remaining_handles) =
            futures::future::select_all(handles).await;
        Ok(Some(completed_selection(
            backend_name,
            result,
            remaining_handles,
        )))
    }

    async fn select(
        &self,
        pending_requests: Vec<PlatformPendingRequest>,
    ) -> Result<PlatformSelectResult, Report<PlatformError>> {
        if pending_requests.is_empty() {
            return Err(Report::new(PlatformError::HttpClient)
                .attach("select called with an empty pending_requests list"));
        }

        let handles = recover_pending_handles(pending_requests)?;

        // Each AxumPendingHandle resolves to (backend_name, result), so the
        // remaining handles keep their own backend names — no positional
        // reconstruction (select_all does not preserve the order of the
        // remaining futures).
        let ((backend_name, result), _ready_idx, remaining_handles) =
            futures::future::select_all(handles).await;

        Ok(completed_selection(backend_name, result, remaining_handles))
    }
}

fn completed_selection(
    backend_name: String,
    result: Result<SpawnedRequestResult, tokio::task::JoinError>,
    remaining_handles: Vec<AxumPendingHandle>,
) -> PlatformSelectResult {
    let remaining: Vec<PlatformPendingRequest> = remaining_handles
        .into_iter()
        .map(|handle| {
            let backend_name = handle.backend_name.clone();
            PlatformPendingRequest::new(handle).with_backend_name(backend_name)
        })
        .collect();

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
        .map(|edge_resp| PlatformResponse::new(edge_resp).with_backend_name(backend_name.clone()));

    // Attribute the failure to its backend so the orchestrator can remove
    // the provider and record a BidStatus::Error, matching the Fastly
    // adapter. Without this, a failed provider silently vanishes through
    // the orchestrator's "backend not identified" branch.
    let failed_backend_name = ready.as_ref().err().map(|_| backend_name);

    PlatformSelectResult {
        ready,
        remaining,
        failed_backend_name,
    }
}

fn recover_pending_handles(
    pending_requests: Vec<PlatformPendingRequest>,
) -> Result<Vec<AxumPendingHandle>, Report<PlatformError>> {
    pending_requests
        .into_iter()
        .map(|pending| {
            pending.downcast::<AxumPendingHandle>().map_err(|_| {
                Report::new(PlatformError::HttpClient)
                    .attach("unexpected pending type in Axum selection")
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// build_runtime_services
// ---------------------------------------------------------------------------

/// Construct [`RuntimeServices`] for an incoming Axum request.
///
/// # Degraded features in dev
///
/// The generic runtime KV slot uses
/// [`trusted_server_core::platform::UnavailableKvStore`]. A `warn` log is
/// emitted once per process.
pub fn build_runtime_services(ctx: &edgezero_core::context::RequestContext) -> RuntimeServices {
    static KV_WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    KV_WARNED.get_or_init(|| {
        log::warn!("Axum dev server: generic runtime KV is unavailable (UnavailableKvStore).");
    });

    let client_ip = edgezero_adapter_axum::context::AxumRequestContext::get(ctx.request())
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

    static TIMER: std::sync::OnceLock<Arc<dyn trusted_server_core::platform::PlatformTimer>> =
        std::sync::OnceLock::new();

    RuntimeServices::builder()
        .timer(Arc::clone(TIMER.get_or_init(|| Arc::new(AxumTimer))))
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
            ..ClientInfo::default()
        })
        .build()
}

struct AxumTimer;

impl trusted_server_core::platform::PlatformTimer for AxumTimer {
    fn delay(&self, duration: std::time::Duration) -> futures::future::LocalBoxFuture<'static, ()> {
        Box::pin(async move {
            tokio::time::sleep(duration).await;
        })
    }
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
    fn html_finalization_timer_is_shared_without_an_executor() {
        let request = edgezero_core::http::request_builder()
            .body(edgezero_core::body::Body::empty())
            .expect("should build a request");
        let context = edgezero_core::context::RequestContext::new(request, Default::default());
        let services = build_runtime_services(&context);
        let next_services = build_runtime_services(&context);
        assert!(
            std::ptr::eq(
                services
                    .timer()
                    .expect("should install a cooperative timer"),
                next_services
                    .timer()
                    .expect("should reuse the cooperative timer"),
            ),
            "should share the stateless timer without requiring an executor"
        );
    }

    #[test]
    fn auction_http_capabilities_are_explicit() {
        let client = AxumPlatformHttpClient::new();
        let capabilities = trusted_server_core::platform::AuctionTargetId::Axum
            .descriptor()
            .capabilities();
        assert!(client.supports_concurrent_fanout());
        assert!(capabilities.supports_concurrent_provider_fanout());
    }

    #[test]
    fn config_env_var_normalizes_store_and_key() {
        assert_eq!(
            config_env_var("my-store.name", "my key"),
            "TRUSTED_SERVER_CONFIG_MY_STORE_NAME_MY_KEY",
            "should normalize environment-variable segments"
        );
    }

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
            between_bytes_timeout: Duration::from_secs(15),
            host_header_override: None,
            discriminator: None,
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
            between_bytes_timeout: Duration::from_secs(15),
            host_header_override: None,
            discriminator: None,
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
    async fn http_client_surfaces_redirect_without_following() {
        let url = serve_raw_response(
            b"HTTP/1.1 302 Found\r\nLocation: https://redirect.example/next\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        let request = edgezero_core::http::request_builder()
            .uri(url)
            .body(EdgeBody::empty())
            .expect("should build outbound request");

        let response = AxumPlatformHttpClient::new()
            .send(PlatformHttpRequest::new(request, "test_backend"))
            .await
            .expect("should surface redirect")
            .response;

        assert_eq!(response.status().as_u16(), 302);
        assert_eq!(
            response
                .headers()
                .get(edgezero_core::http::header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("https://redirect.example/next")
        );
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
            response
                .into_body()
                .into_bytes()
                .unwrap_or_default()
                .as_ref(),
            b"ok",
            "should preserve decoded response body"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn select_attributes_failed_backend_name() {
        // Bind and immediately drop a listener so the port is closed — the
        // request fails with connection refused.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("should bind probe listener");
        let addr = listener.local_addr().expect("should read local address");
        drop(listener);

        let request = edgezero_core::http::request_builder()
            .uri(format!("http://{addr}/"))
            .body(EdgeBody::empty())
            .expect("should build outbound request");

        let client = AxumPlatformHttpClient::new();
        let pending = client
            .send_async(PlatformHttpRequest::new(request, "failing_backend"))
            .await
            .expect("should spawn async request");

        let result = client
            .select(vec![pending])
            .await
            .expect("select should surface the failure via ready, not a fatal error");

        assert!(
            result.ready.is_err(),
            "request to a closed port should fail"
        );
        assert_eq!(
            result.failed_backend_name.as_deref(),
            Some("failing_backend"),
            "failed provider must be attributed to its backend so the orchestrator can record BidStatus::Error"
        );
        assert!(
            result.remaining.is_empty(),
            "no remaining requests expected"
        );
    }

    async fn deadline_pending(
        client: &AxumPlatformHttpClient,
        url: String,
        backend: &str,
    ) -> PlatformPendingRequest {
        let request = edgezero_core::http::request_builder()
            .uri(url)
            .body(EdgeBody::empty())
            .expect("should build deadline request");
        client
            .send_async(PlatformHttpRequest::new(request, backend))
            .await
            .expect("should dispatch deadline request")
    }

    async fn serve_slow_response(trickle_body: bool) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("should bind deadline server");
        let address = listener
            .local_addr()
            .expect("should read deadline server address");
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener
                .accept()
                .await
                .expect("should accept deadline request");
            let mut request = [0; 1024];
            let bytes_read = stream
                .read(&mut request)
                .await
                .expect("should read deadline request");
            assert!(bytes_read > 0, "should receive a deadline request");
            if trickle_body {
                if stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                    .await
                    .is_err()
                {
                    return;
                }
                for _ in 0..100 {
                    if stream.write_all(b"x").await.is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            } else {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await;
            }
        });
        (format!("http://{address}/"), task)
    }

    #[tokio::test]
    async fn select_until_bounds_delayed_headers_and_trickling_bodies() {
        let client = AxumPlatformHttpClient::new();
        for trickle_body in [false, true] {
            let (url, server) = serve_slow_response(trickle_body).await;
            let pending = deadline_pending(&client, url, "slow").await;
            let start = web_time::Instant::now();
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                client.select_until(vec![pending], start + Duration::from_millis(200)),
            )
            .await
            .expect("should finish before the slow response")
            .expect("should enforce the deadline");
            server.abort();
            assert!(
                result.is_none(),
                "should time out before the entire response completes"
            );
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "should bound the complete response wait"
            );
        }
    }

    #[tokio::test]
    async fn select_until_preserves_fast_success_before_slow_body_expires() {
        let client = AxumPlatformHttpClient::new();
        let (slow_url, server) = serve_slow_response(true).await;
        let fast_url = serve_raw_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;
        let slow = deadline_pending(&client, slow_url, "slow").await;
        let fast = deadline_pending(&client, fast_url, "fast").await;
        let deadline = web_time::Instant::now() + Duration::from_millis(500);
        let result = client
            .select_until(vec![slow, fast], deadline)
            .await
            .expect("should collect fast response")
            .expect("should complete before the deadline");
        assert_eq!(
            result
                .ready
                .expect("should succeed")
                .backend_name
                .as_deref(),
            Some("fast"),
            "should collect the fast complete body first"
        );
        assert_eq!(
            result.remaining.len(),
            1,
            "should preserve the slow body handle"
        );
        let result = client
            .select_until(result.remaining, deadline)
            .await
            .expect("should bound slow response");
        server.abort();
        assert!(result.is_none(), "should expire the remaining slow body");
    }

    #[tokio::test]
    async fn select_until_expired_deadline_preserves_buffered_response() {
        let client = AxumPlatformHttpClient::new();
        let handle = tokio::spawn(async { Ok((200, Vec::new(), b"ok".to_vec())) });
        while !handle.is_finished() {
            tokio::task::yield_now().await;
        }
        let pending = PlatformPendingRequest::new(AxumPendingHandle {
            backend_name: "ready".to_owned(),
            handle,
        });
        let result = client
            .select_until(vec![pending], web_time::Instant::now())
            .await
            .expect("should recognize expired deadline");
        assert!(
            result.is_some(),
            "should preserve a complete buffered response without waiting after cutoff"
        );
    }

    #[tokio::test]
    async fn expired_snapshot_preserves_finished_siblings_and_aborts_unfinished_task() {
        let mut tasks = Vec::new();
        for name in ["ready-a", "ready-b"] {
            tasks.push(AxumPendingHandle {
                backend_name: name.to_owned(),
                handle: tokio::spawn(async { Ok((200, Vec::new(), b"ok".to_vec())) }),
            });
        }
        while tasks.iter().any(|pending| !pending.handle.is_finished()) {
            tokio::task::yield_now().await;
        }
        let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
        let (drop_sender, drop_receiver) = tokio::sync::oneshot::channel::<()>();
        tasks.push(AxumPendingHandle {
            backend_name: "unfinished".to_owned(),
            handle: tokio::spawn(async move {
                let _drop_signal = drop_sender;
                started_sender
                    .send(())
                    .expect("should signal unfinished task start");
                futures::future::pending::<SpawnedRequestResult>().await
            }),
        });
        started_receiver
            .await
            .expect("should start unfinished task");
        let pending = tasks
            .into_iter()
            .map(|task| {
                let name = task.backend_name.clone();
                PlatformPendingRequest::new(task).with_backend_name(name)
            })
            .collect();
        let client = AxumPlatformHttpClient::new();
        let deadline = web_time::Instant::now();
        let first = client
            .select_until(pending, deadline)
            .await
            .expect("should snapshot ready responses")
            .expect("should retain first response");
        assert!(
            first.ready.is_ok(),
            "should preserve first completed result"
        );
        assert_eq!(
            first.remaining.len(),
            1,
            "should retain only the other completed response"
        );
        let second = client
            .select_until(first.remaining, deadline)
            .await
            .expect("should drain the completed snapshot")
            .expect("should retain second response");
        assert!(
            second.ready.is_ok(),
            "should preserve the second completed result"
        );
        assert!(
            second.remaining.is_empty(),
            "should leave no unfinished handles"
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(1), drop_receiver)
                .await
                .expect("should promptly abort unfinished work")
                .is_err(),
            "should release unfinished task resources"
        );
    }

    #[tokio::test]
    async fn select_until_aborts_task_on_expiration() {
        let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
        let (dropped_sender, dropped_receiver) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let _drop_signal = dropped_sender;
            started_sender.send(()).expect("should signal task start");
            futures::future::pending::<SpawnedRequestResult>().await
        });
        started_receiver
            .await
            .expect("should start task before selection");
        let pending = PlatformPendingRequest::new(AxumPendingHandle {
            backend_name: "slow".to_owned(),
            handle,
        });
        let result = AxumPlatformHttpClient::new()
            .select_until(
                vec![pending],
                web_time::Instant::now() + Duration::from_millis(50),
            )
            .await
            .expect("should expire task");
        assert!(result.is_none(), "should report deadline expiration");
        assert!(
            tokio::time::timeout(Duration::from_secs(2), dropped_receiver)
                .await
                .expect("should promptly drop aborted task state")
                .is_err(),
            "should drop task resources on expiration"
        );
    }

    #[tokio::test]
    async fn select_until_preserves_failed_backend_attribution() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("should bind probe");
        let address = listener.local_addr().expect("should read probe address");
        drop(listener);
        let client = AxumPlatformHttpClient::new();
        let pending = deadline_pending(&client, format!("http://{address}/"), "failed").await;
        let result = client
            .select_until(
                vec![pending],
                web_time::Instant::now() + Duration::from_secs(2),
            )
            .await
            .expect("should collect transport failure")
            .expect("should fail before deadline");
        assert!(result.ready.is_err(), "should preserve transport failure");
        assert_eq!(
            result.failed_backend_name.as_deref(),
            Some("failed"),
            "should attribute failure to its backend"
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
