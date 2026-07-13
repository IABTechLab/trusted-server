use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use edgezero_core::http::{HeaderMap, HeaderName, HeaderValue, header};
use edgezero_core::store_registry::{ConfigRegistry, KvRegistry, SecretRegistry};
use error_stack::{Report, ResultExt as _};
use trusted_server_core::platform::{
    ClientInfo, CompositeConfigStore, CompositeSecretStore, GeoInfo, PlatformBackend,
    PlatformBackendSpec, PlatformConfigStore, PlatformConfigWriter, PlatformError, PlatformGeo,
    PlatformHttpClient, PlatformHttpRequest, PlatformPendingRequest, PlatformResponse,
    PlatformSecretStore, PlatformSecretWriter, PlatformSelectResult, RuntimeServices, StoreId,
    StoreName,
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

impl PlatformConfigWriter for AxumPlatformConfigStore {
    fn put(&self, store_id: &StoreId, key: &str, value: &str) -> Result<(), Report<PlatformError>> {
        PlatformConfigStore::put(self, store_id, key, value)
    }

    fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
        PlatformConfigStore::delete(self, store_id, key)
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

impl PlatformSecretWriter for AxumPlatformSecretStore {
    fn create(
        &self,
        store_id: &StoreId,
        name: &str,
        value: &str,
    ) -> Result<(), Report<PlatformError>> {
        PlatformSecretStore::create(self, store_id, name, value)
    }

    fn delete(&self, store_id: &StoreId, name: &str) -> Result<(), Report<PlatformError>> {
        PlatformSecretStore::delete(self, store_id, name)
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

    async fn select(
        &self,
        pending_requests: Vec<PlatformPendingRequest>,
    ) -> Result<PlatformSelectResult, Report<PlatformError>> {
        if pending_requests.is_empty() {
            return Err(Report::new(PlatformError::HttpClient)
                .attach("select called with an empty pending_requests list"));
        }

        let handles: Vec<AxumPendingHandle> = pending_requests
            .into_iter()
            .map(|pr| {
                pr.downcast::<AxumPendingHandle>().map_err(|_| {
                    Report::new(PlatformError::HttpClient)
                        .attach("unexpected inner type in AxumPlatformHttpClient::select")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Each AxumPendingHandle resolves to (backend_name, result), so the
        // remaining handles keep their own backend names — no positional
        // reconstruction (select_all does not preserve the order of the
        // remaining futures).
        let ((backend_name, result), _ready_idx, remaining_handles) =
            futures::future::select_all(handles).await;

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
            .map(|edge_resp| {
                PlatformResponse::new(edge_resp).with_backend_name(backend_name.clone())
            });

        // Attribute the failure to its backend so the orchestrator can remove
        // the provider and record a BidStatus::Error, matching the Fastly
        // adapter. Without this, a failed provider silently vanishes through
        // the orchestrator's "backend not identified" branch.
        let failed_backend_name = ready.as_ref().err().map(|_| backend_name);

        Ok(PlatformSelectResult {
            ready,
            remaining,
            failed_backend_name,
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
pub fn build_runtime_services(ctx: &edgezero_core::context::RequestContext) -> RuntimeServices {
    static KV_WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    KV_WARNED.get_or_init(|| {
        log::warn!(
            "Axum dev server: KV store is unavailable (UnavailableKvStore). \
             Routes that depend on synthetic-ID or consent KV will degrade gracefully."
        );
    });

    let client_ip = edgezero_adapter_axum::context::AxumRequestContext::get(ctx.request())
        .and_then(|c| c.remote_addr)
        .map(|addr| addr.ip());

    use trusted_server_core::platform::{
        PlatformBackend, PlatformConfigWriter, PlatformGeo, PlatformKvStore, PlatformSecretWriter,
    };

    // Stateless shims are promoted to process-wide statics so callers clone
    // an existing Arc instead of allocating a new one per request.
    static CONFIG_WRITER: std::sync::OnceLock<Arc<dyn PlatformConfigWriter>> =
        std::sync::OnceLock::new();
    static SECRET_WRITER: std::sync::OnceLock<Arc<dyn PlatformSecretWriter>> =
        std::sync::OnceLock::new();
    static KV_STORE: std::sync::OnceLock<Arc<dyn PlatformKvStore>> = std::sync::OnceLock::new();
    static BACKEND: std::sync::OnceLock<Arc<dyn PlatformBackend>> = std::sync::OnceLock::new();
    static GEO: std::sync::OnceLock<Arc<dyn PlatformGeo>> = std::sync::OnceLock::new();

    // Config/secret reads resolve through the whole registry (from request
    // extensions, inserted by the AxumDevServer registry setters) so non-default
    // logical ids resolve; writes delegate to the env-backed dev-server stores
    // (which reject writes). An absent registry makes composite reads error
    // rather than silently reading a default store.
    let config_reader = ctx.request().extensions().get::<ConfigRegistry>().cloned();
    let secret_reader = ctx.request().extensions().get::<SecretRegistry>().cloned();
    let kv_registry = ctx.request().extensions().get::<KvRegistry>().cloned();
    let config_writer = Arc::clone(
        CONFIG_WRITER
            .get_or_init(|| Arc::new(AxumPlatformConfigStore) as Arc<dyn PlatformConfigWriter>),
    );
    let secret_writer = Arc::clone(
        SECRET_WRITER
            .get_or_init(|| Arc::new(AxumPlatformSecretStore) as Arc<dyn PlatformSecretWriter>),
    );

    RuntimeServices::builder()
        .config_store(Arc::new(CompositeConfigStore::new(
            config_reader,
            config_writer,
        )))
        .secret_store(Arc::new(CompositeSecretStore::new(
            secret_reader,
            secret_writer,
        )))
        .kv_store(Arc::clone(KV_STORE.get_or_init(|| {
            Arc::new(trusted_server_core::platform::UnavailableKvStore) as Arc<dyn PlatformKvStore>
        })))
        .kv_registry(kv_registry)
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod registry_test_support {
    //! In-memory store doubles and a `RequestContext` builder that seeds the
    //! three `EdgeZero` registries into request extensions, so adapter tests can
    //! exercise non-default logical store resolution through the composite.

    use std::collections::{BTreeMap, HashMap};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    // `bytes` is not a direct dependency of this crate; use the re-export from the
    // `axum` dev-dependency so no new dependency edge (and Cargo.lock churn) is
    // introduced for test-only code.
    use axum::body::Bytes;
    use edgezero_core::config_store::{ConfigStore, ConfigStoreError, ConfigStoreHandle};
    use edgezero_core::context::RequestContext;
    use edgezero_core::http::request_builder;
    use edgezero_core::key_value_store::{KvError, KvHandle, KvPage, KvStore};
    use edgezero_core::params::PathParams;
    use edgezero_core::secret_store::{SecretError, SecretHandle, SecretStore};
    use edgezero_core::store_registry::{
        BoundSecretStore, ConfigRegistry, ConfigStoreBinding, KvRegistry, SecretRegistry,
        StoreRegistry,
    };

    /// In-memory [`ConfigStore`] double keyed by lookup key.
    struct MemConfigStore {
        data: HashMap<String, String>,
    }

    #[async_trait(?Send)]
    impl ConfigStore for MemConfigStore {
        async fn get(&self, key: &str) -> Result<Option<String>, ConfigStoreError> {
            Ok(self.data.get(key).cloned())
        }
    }

    /// In-memory [`SecretStore`] double keyed by `"{store_name}/{key}"`.
    struct MemSecretStore {
        data: HashMap<String, Bytes>,
    }

    #[async_trait(?Send)]
    impl SecretStore for MemSecretStore {
        async fn get_bytes(
            &self,
            store_name: &str,
            key: &str,
        ) -> Result<Option<Bytes>, SecretError> {
            Ok(self.data.get(&format!("{store_name}/{key}")).cloned())
        }
    }

    /// In-memory [`KvStore`] double.
    #[derive(Default)]
    struct MemKvStore {
        data: Mutex<HashMap<String, Bytes>>,
    }

    #[async_trait(?Send)]
    impl KvStore for MemKvStore {
        async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
            Ok(self.data.lock().expect("should lock").get(key).cloned())
        }

        async fn put_bytes(&self, key: &str, value: Bytes) -> Result<(), KvError> {
            self.data
                .lock()
                .expect("should lock")
                .insert(key.to_owned(), value);
            Ok(())
        }

        async fn put_bytes_with_ttl(
            &self,
            key: &str,
            value: Bytes,
            _ttl: std::time::Duration,
        ) -> Result<(), KvError> {
            self.put_bytes(key, value).await
        }

        async fn delete(&self, key: &str) -> Result<(), KvError> {
            self.data.lock().expect("should lock").remove(key);
            Ok(())
        }

        async fn list_keys_page(
            &self,
            _prefix: &str,
            _cursor: Option<&str>,
            _limit: usize,
        ) -> Result<KvPage, KvError> {
            Ok(KvPage::default())
        }
    }

    /// Build a [`ConfigRegistry`] from `(store_id, key, value)` entries.
    pub(super) fn config_registry(entries: &[(&str, &str, &str)], default: &str) -> ConfigRegistry {
        let mut by_store: BTreeMap<String, HashMap<String, String>> = BTreeMap::new();
        for (id, key, value) in entries {
            by_store
                .entry((*id).to_owned())
                .or_default()
                .insert((*key).to_owned(), (*value).to_owned());
        }
        let by_id: BTreeMap<String, ConfigStoreBinding> = by_store
            .into_iter()
            .map(|(id, data)| {
                let binding = ConfigStoreBinding {
                    default_key: id.clone(),
                    handle: ConfigStoreHandle::new(Arc::new(MemConfigStore { data })),
                };
                (id, binding)
            })
            .collect();
        StoreRegistry::from_parts(by_id, default.to_owned())
            .expect("should build a non-empty config registry")
    }

    /// Build a [`SecretRegistry`] from `(store_id, key, value)` entries.
    pub(super) fn secret_registry(
        entries: &[(&str, &str, &[u8])],
        default: &str,
    ) -> SecretRegistry {
        let mut data: HashMap<String, Bytes> = HashMap::new();
        let mut ids: BTreeMap<String, ()> = BTreeMap::new();
        for (id, key, value) in entries {
            data.insert(format!("{id}/{key}"), Bytes::copy_from_slice(value));
            ids.insert((*id).to_owned(), ());
        }
        let handle = SecretHandle::new(Arc::new(MemSecretStore { data }));
        let by_id: BTreeMap<String, BoundSecretStore> = ids
            .into_keys()
            .map(|id| {
                let bound = BoundSecretStore::new(handle.clone(), id.clone());
                (id, bound)
            })
            .collect();
        StoreRegistry::from_parts(by_id, default.to_owned())
            .expect("should build a non-empty secret registry")
    }

    /// Build a [`KvRegistry`] from `(store_id, key, value)` entries; each id maps
    /// to its own in-memory store so distinct ids are observably distinct.
    pub(super) fn kv_registry(entries: &[(&str, &str, &[u8])], default: &str) -> KvRegistry {
        let mut by_store: BTreeMap<String, Arc<MemKvStore>> = BTreeMap::new();
        for (id, key, value) in entries {
            let store = by_store.entry((*id).to_owned()).or_default();
            store
                .data
                .lock()
                .expect("should lock")
                .insert((*key).to_owned(), Bytes::copy_from_slice(value));
        }
        let by_id: BTreeMap<String, KvHandle> = by_store
            .into_iter()
            .map(|(id, store)| (id, KvHandle::new(store)))
            .collect();
        StoreRegistry::from_parts(by_id, default.to_owned())
            .expect("should build a non-empty kv registry")
    }

    /// Build a [`RequestContext`] with the three registries inserted into request
    /// extensions, mirroring the dev server's registry wiring.
    pub(super) fn test_context_with_registries(
        config: Option<ConfigRegistry>,
        kv: Option<KvRegistry>,
        secrets: Option<SecretRegistry>,
    ) -> RequestContext {
        let mut builder = request_builder().method("GET").uri("https://example.com/");
        if let Some(config) = config {
            builder = builder.extension(config);
        }
        if let Some(kv) = kv {
            builder = builder.extension(kv);
        }
        if let Some(secrets) = secrets {
            builder = builder.extension(secrets);
        }
        let req = builder
            .body(edgezero_core::body::Body::empty())
            .expect("should build test request");
        RequestContext::new(req, PathParams::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edgezero_core::body::Body as EdgeBody;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::registry_test_support::{
        config_registry, kv_registry, secret_registry, test_context_with_registries,
    };

    #[test]
    fn config_store_resolves_nondefault_jwks_store() {
        // Arrange: registry with the default config store plus a non-default
        // `jwks_store` (D5: default config id is `trusted_server_config`).
        let config = config_registry(
            &[
                ("trusted_server_config", "current-kid", "kid-1"),
                ("jwks_store", "kid-1", "{\"kty\":\"OKP\"}"),
            ],
            "trusted_server_config",
        );
        let ctx = test_context_with_registries(Some(config), None, None);
        let services = build_runtime_services(&ctx);

        // Act + Assert: the non-default config id resolves through the composite.
        let jwk = services
            .config_store()
            .get(&StoreName::from("jwks_store"), "kid-1")
            .expect("should resolve the non-default jwks_store through the composite");
        assert_eq!(
            jwk, "{\"kty\":\"OKP\"}",
            "should read the seeded value from the non-default config store"
        );

        // Unknown id is a strict error, never a silent fallback.
        assert!(
            services
                .config_store()
                .get(&StoreName::from("no_such_store"), "kid-1")
                .is_err(),
            "unknown config id should error, not fall back to the default store"
        );
    }

    #[test]
    fn secret_store_resolves_nondefault_ts_secrets_and_s3_auth() {
        // Arrange: registry with the default secret store plus non-default
        // `ts_secrets` (DataDome) and `s3_auth` (S3 SigV4) ids.
        let secrets = secret_registry(
            &[
                ("trusted_server_secrets", "API_KEY", b"default-key"),
                ("ts_secrets", "server-side-key", b"dd-secret"),
                ("s3_auth", "aws-secret-access-key", b"s3-secret"),
            ],
            "trusted_server_secrets",
        );
        let ctx = test_context_with_registries(None, None, Some(secrets));
        let services = build_runtime_services(&ctx);

        let dd = services
            .secret_store()
            .get_bytes(&StoreName::from("ts_secrets"), "server-side-key")
            .expect("should resolve ts_secrets through the composite");
        assert_eq!(dd, b"dd-secret", "should read the seeded DataDome secret");

        let s3 = services
            .secret_store()
            .get_bytes(&StoreName::from("s3_auth"), "aws-secret-access-key")
            .expect("should resolve s3_auth through the composite");
        assert_eq!(s3, b"s3-secret", "should read the seeded S3 secret");

        assert!(
            services
                .secret_store()
                .get_bytes(&StoreName::from("no_such_store"), "x")
                .is_err(),
            "unknown secret id should error, not fall back to the default store"
        );
    }

    #[tokio::test]
    async fn kv_handle_named_resolves_consent_store() {
        // Arrange: registry with the default KV store plus a non-default
        // `consent_store`, each carrying a different value for the same key.
        let kv = kv_registry(
            &[
                ("trusted_server_kv", "marker", b"default-value"),
                ("consent_store", "marker", b"consent-value"),
            ],
            "trusted_server_kv",
        );
        let ctx = test_context_with_registries(None, Some(kv), None);
        let services = build_runtime_services(&ctx);

        // The named store resolves and is distinct from the default request-path
        // KV store (which is the dev server's unavailable store).
        let handle = services
            .kv_handle_named("consent_store")
            .expect("should resolve the consent_store handle");
        let value = handle
            .get_bytes("marker")
            .await
            .expect("should read from consent_store")
            .expect("should find the seeded key");
        assert_eq!(
            value.as_ref(),
            b"consent-value",
            "named lookup should read the consent_store value, not the default"
        );
        assert!(
            services.kv_handle().get_bytes("marker").await.is_err(),
            "the default request-path KV store is distinct from consent_store"
        );

        // Unknown id yields None.
        assert!(
            services.kv_handle_named("no_such_store").is_none(),
            "unknown KV id should resolve to None"
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
