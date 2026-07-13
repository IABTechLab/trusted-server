use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use edgezero_core::config_store::ConfigStoreHandle;
use edgezero_core::key_value_store::{KvHandle, KvPage, KvStore};
use edgezero_core::store_registry::{ConfigRegistry, KvRegistry, SecretRegistry};
use error_stack::Report;
use trusted_server_core::platform::{
    ClientInfo, CompositeConfigStore, CompositeSecretStore, GeoInfo, KvError, PlatformBackend,
    PlatformBackendSpec, PlatformConfigStore, PlatformConfigWriter, PlatformError, PlatformGeo,
    PlatformHttpClient, PlatformKvStore, PlatformSecretStore, PlatformSecretWriter,
    RuntimeServices, StoreId, StoreName, UnavailableKvStore,
};

#[cfg(not(target_arch = "wasm32"))]
use trusted_server_core::platform::UnavailableHttpClient;

#[cfg(target_arch = "wasm32")]
use error_stack::ResultExt as _;
#[cfg(target_arch = "wasm32")]
use trusted_server_core::platform::{
    PlatformHttpRequest, PlatformPendingRequest, PlatformResponse, PlatformSelectResult,
};

// ---------------------------------------------------------------------------
// Noop stubs — used when a handle is absent (native CI, missing binding)
// ---------------------------------------------------------------------------

struct NoopConfigStore;

impl PlatformConfigStore for NoopConfigStore {
    fn get(&self, _: &StoreName, _: &str) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("config store not available"))
    }

    fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("config store not available"))
    }

    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("config store not available"))
    }
}

impl PlatformConfigWriter for NoopConfigStore {
    fn put(&self, store_id: &StoreId, key: &str, value: &str) -> Result<(), Report<PlatformError>> {
        PlatformConfigStore::put(self, store_id, key, value)
    }

    fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
        PlatformConfigStore::delete(self, store_id, key)
    }
}

struct NoopSecretStore;

impl PlatformSecretStore for NoopSecretStore {
    fn get_bytes(&self, _: &StoreName, _: &str) -> Result<Vec<u8>, Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore).attach("secret store not available"))
    }

    fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore).attach("secret store not available"))
    }

    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore).attach("secret store not available"))
    }
}

impl PlatformSecretWriter for NoopSecretStore {
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

struct NoopBackend;

impl PlatformBackend for NoopBackend {
    fn predict_name(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        let port = spec
            .port
            .unwrap_or(if spec.scheme == "https" { 443 } else { 80 });
        let timeout_ms = spec.first_byte_timeout.as_millis();
        let cert_suffix = if spec.certificate_check {
            ""
        } else {
            "_nocert"
        };
        Ok(format!(
            "{}_{}_{}_{timeout_ms}ms{cert_suffix}",
            spec.scheme, spec.host, port
        ))
    }

    fn ensure(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        self.predict_name(spec)
    }
}

// ---------------------------------------------------------------------------
// edgezero handle adapters — no #[cfg] needed; platform-specific store
// construction is handled by edgezero's run_app before we receive the ctx.
// ---------------------------------------------------------------------------

/// Bridges edgezero's [`ConfigStoreHandle`] (injected by `run_app` from the
/// `TRUSTED_SERVER_CONFIG` env-var binding) to [`PlatformConfigStore`].
///
/// Reads delegate through the handle. Writes are unsupported on all current
/// adapter targets and return errors.
///
/// Note: Cloudflare config is a single flat JSON env-var binding — all keys
/// live in one namespace. The `store_name` argument is intentionally ignored;
/// callers cannot route to a different store by passing a different name.
struct ConfigStoreHandleAdapter(ConfigStoreHandle);

impl PlatformConfigStore for ConfigStoreHandleAdapter {
    fn get(&self, _store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        futures::executor::block_on(self.0.get(key))
            .map_err(|e| {
                Report::new(PlatformError::ConfigStore)
                    .attach(format!("config store lookup failed: {e}"))
            })?
            .ok_or_else(|| {
                Report::new(PlatformError::ConfigStore).attach(format!("key not found: {key}"))
            })
    }

    fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("config store writes are not supported"))
    }

    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("config store writes are not supported"))
    }
}

impl PlatformConfigWriter for ConfigStoreHandleAdapter {
    fn put(&self, store_id: &StoreId, key: &str, value: &str) -> Result<(), Report<PlatformError>> {
        PlatformConfigStore::put(self, store_id, key, value)
    }

    fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
        PlatformConfigStore::delete(self, store_id, key)
    }
}

/// Bridges edgezero's [`KvHandle`] (injected by `run_app` from the
/// `TRUSTED_SERVER_KV` KV namespace binding) to [`PlatformKvStore`].
///
/// Delegates all operations through `KvHandle`'s raw-bytes API, which includes
/// key/value validation before forwarding to the underlying store.
///
/// Note: key/value validation runs twice — once inside this `KvHandle` and once
/// inside the `KvHandle` that `RuntimeServices::kv_handle()` constructs from
/// this adapter. The overhead is negligible (string length checks only) and
/// avoided by the fact that we reuse the already-opened `env.kv()` handle from
/// `run_app` rather than opening a new one.
struct KvHandleAdapter(KvHandle);

#[async_trait::async_trait(?Send)]
impl KvStore for KvHandleAdapter {
    async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
        self.0.get_bytes(key).await
    }

    async fn put_bytes(&self, key: &str, value: Bytes) -> Result<(), KvError> {
        self.0.put_bytes(key, value).await
    }

    async fn put_bytes_with_ttl(
        &self,
        key: &str,
        value: Bytes,
        ttl: Duration,
    ) -> Result<(), KvError> {
        self.0.put_bytes_with_ttl(key, value, ttl).await
    }

    async fn delete(&self, key: &str) -> Result<(), KvError> {
        self.0.delete(key).await
    }

    async fn list_keys_page(
        &self,
        prefix: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<KvPage, KvError> {
        self.0.list_keys_page(prefix, cursor, limit).await
    }
}

// ---------------------------------------------------------------------------
// CloudflareHttpClient — WASM target only
// ---------------------------------------------------------------------------

/// Carries a completed response through `send_async` → `select`.
///
/// Same pattern as `AxumPendingResponse`: stores raw parts because `Body::Stream`
/// is `!Send`, which is incompatible with `Box<dyn Any + Send>` inside
/// [`PlatformPendingRequest`].
#[cfg(target_arch = "wasm32")]
struct CloudflarePendingResponse {
    backend_name: String,
    status: u16,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
}

/// [`worker::Fetch`]-backed HTTP client for the Cloudflare Workers runtime.
///
/// # Multi-provider auction limitation
///
/// `send_async` eagerly awaits each request before returning, so
/// [`PlatformHttpClient::supports_concurrent_fanout`] reports `false` and the
/// auction orchestrator rejects multi-provider configurations before any
/// request launches. `select` keeps a defense-in-depth rejection for more
/// than one pending request. Configure a single auction provider for
/// Cloudflare Workers, or use the Fastly adapter for parallel DSP fan-out.
///
/// Per-provider timeouts baked into the backend name are not enforced at the
/// fetch layer; the Workers runtime's global CPU budget (~30 s on paid plans)
/// is the only implicit deadline.
#[cfg(target_arch = "wasm32")]
pub struct CloudflareHttpClient;

/// Maximum buffered upstream response body, mirroring the Fastly adapter's cap.
///
/// Cloudflare Workers isolates have a bounded memory budget (~128 MB), so a
/// large or hostile upstream could OOM the isolate. `execute` buffers the whole
/// body via `resp.bytes()`, so it caps both the declared Content-Length and the
/// materialized byte count at this limit.
#[cfg(target_arch = "wasm32")]
const MAX_PLATFORM_RESPONSE_BODY_BYTES: usize = 10 * 1024 * 1024; // 10 MiB

/// Collect the lowercased tokens listed in the upstream `Connection:` header(s).
///
/// Every header named here is hop-by-hop (RFC 7230 §6.1) and must not be
/// forwarded downstream.
#[cfg(target_arch = "wasm32")]
fn response_connection_tokens(resp: &worker::Response) -> Vec<String> {
    resp.headers()
        .entries()
        .filter(|(name, _)| name.eq_ignore_ascii_case("connection"))
        .flat_map(|(_, value)| {
            value
                .split(',')
                .map(|token| token.trim().to_ascii_lowercase())
                .filter(|token| !token.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Returns `true` for hop-by-hop response headers that must not be forwarded
/// downstream (RFC 7230 §6.1), including any header named in the upstream
/// `Connection:` token list. Mirrors the Axum adapter's
/// `is_hop_by_hop_response_header`. `transfer-encoding` is stripped separately
/// at the call site alongside the other auto-decoded framing headers.
#[cfg(target_arch = "wasm32")]
fn is_hop_by_hop_response_header(name: &str, connection_tokens: &[String]) -> bool {
    const HOP_BY_HOP: [&str; 6] = [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "upgrade",
    ];
    let lower = name.to_ascii_lowercase();
    HOP_BY_HOP.iter().any(|header| *header == lower) || connection_tokens.contains(&lower)
}

#[cfg(target_arch = "wasm32")]
impl CloudflareHttpClient {
    async fn execute(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        use worker::{Fetch, Headers, Method, Request, RequestInit, RequestRedirect};

        // The Cloudflare fetch path cannot honor Fastly-style Image Optimizer
        // metadata, and it always buffers the response body (see below). The
        // `PlatformHttpRequest` contract requires adapters that cannot honor
        // these to reject rather than silently drop the behavior, so surface a
        // typed error instead of returning an untransformed or fully-buffered
        // response. These fields are only set by asset routes, which are not
        // routed to the Cloudflare adapter today; the guard keeps the contract
        // honest if that changes.
        if request.image_optimizer.is_some() {
            return Err(Report::new(PlatformError::HttpClient)
                .attach("Image Optimizer is not supported on the Cloudflare Workers runtime"));
        }
        if request.stream_response {
            return Err(Report::new(PlatformError::HttpClient).attach(
                "streaming response bodies are not supported on the Cloudflare Workers runtime",
            ));
        }

        let uri = request.request.uri().to_string();
        // http::Method always stores uppercase; worker 0.7 implements From<String> only.
        let method = Method::from(request.request.method().to_string());

        let headers = Headers::new();
        for (name, value) in request.request.headers() {
            let value_str = std::str::from_utf8(value.as_bytes())
                .change_context(PlatformError::HttpClient)
                .attach_with(|| {
                    format!("non-UTF-8 bytes in outbound header `{name}` — value dropped")
                })?;
            // `append` rather than `set`: a request carrying the same header
            // name more than once must forward every value, matching the
            // response path which appends via `edge_builder.header(...)`.
            headers
                .append(name.as_str(), value_str)
                .change_context(PlatformError::HttpClient)?;
        }

        let (_, body) = request.request.into_parts();
        let body_bytes = match body {
            edgezero_core::body::Body::Once(bytes) => bytes.to_vec(),
            edgezero_core::body::Body::Stream(_) => {
                return Err(Report::new(PlatformError::HttpClient)
                    .attach("streaming request bodies are not supported on Cloudflare Workers"));
            }
        };

        let mut init = RequestInit::new();
        // Force manual redirect handling: the Workers runtime otherwise defaults
        // to `RequestRedirect::Follow` and transparently chases 3xx responses to
        // any host inside `Fetch::send()`. Core's `proxy_with_redirects` does its
        // own per-hop redirect handling and validates each next hop against
        // `allowed_domains`; auto-following here would bypass that allowlist
        // (SSRF). `Manual` surfaces the 3xx + Location back to core unfollowed,
        // matching the Axum adapter's `redirect::Policy::none()`.
        init.with_method(method)
            .with_headers(headers)
            .with_redirect(RequestRedirect::Manual);
        if !body_bytes.is_empty() {
            let uint8 = js_sys::Uint8Array::from(body_bytes.as_slice());
            init.with_body(Some(uint8.into()));
        }

        let worker_req =
            Request::new_with_init(&uri, &init).change_context(PlatformError::HttpClient)?;

        let mut resp = Fetch::Request(worker_req)
            .send()
            .await
            .change_context(PlatformError::HttpClient)
            .attach_with(|| format!("outbound request to {uri} failed"))?;

        let status = resp.status_code();

        // Pre-flight: reject oversized responses before copying bytes into the
        // isolate heap. Content-Length is advisory (and absent on chunked
        // responses), so the post-buffer check below is the real guard; this
        // just rejects honestly-declared large bodies cheaply. Mirrors the
        // Fastly adapter's two-stage cap.
        if let Some(claimed_len) = resp
            .headers()
            .get("content-length")
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse::<usize>().ok())
            && claimed_len > MAX_PLATFORM_RESPONSE_BODY_BYTES
        {
            return Err(Report::new(PlatformError::HttpClient).attach(format!(
                "origin Content-Length {claimed_len} exceeds \
                 {MAX_PLATFORM_RESPONSE_BODY_BYTES}-byte response body limit"
            )));
        }

        let connection_tokens = response_connection_tokens(&resp);
        let mut edge_builder = edgezero_core::http::response_builder().status(status);
        for (name, value) in resp.headers().entries() {
            // The Workers runtime auto-decompresses gzip/br/deflate and handles
            // chunked transfer — strip these headers so the proxy layer does not
            // attempt a second decompression pass on the already-decoded body.
            // Content-Length is stripped too: the origin value describes the
            // compressed payload, not the decoded bytes returned by `bytes()`,
            // and forwarding it stale would truncate pass-through responses.
            // The accurate length is set from the decoded body below.
            if name.eq_ignore_ascii_case("content-encoding")
                || name.eq_ignore_ascii_case("transfer-encoding")
                || name.eq_ignore_ascii_case("content-length")
            {
                continue;
            }
            // Strip hop-by-hop headers (and any header named in the upstream
            // `Connection:` token list) so they are not forwarded downstream,
            // matching the Axum adapter's `is_hop_by_hop_response_header`.
            if is_hop_by_hop_response_header(&name, &connection_tokens) {
                continue;
            }
            edge_builder = edge_builder.header(name.as_str(), value.as_bytes());
        }
        let body_bytes = resp
            .bytes()
            .await
            .change_context(PlatformError::HttpClient)?;

        // Belt-and-suspenders: catches chunked responses with no Content-Length.
        if body_bytes.len() > MAX_PLATFORM_RESPONSE_BODY_BYTES {
            return Err(Report::new(PlatformError::HttpClient).attach(format!(
                "origin response body {} bytes exceeds \
                 {MAX_PLATFORM_RESPONSE_BODY_BYTES}-byte limit",
                body_bytes.len()
            )));
        }
        edge_builder = edge_builder.header(
            edgezero_core::http::header::CONTENT_LENGTH,
            body_bytes.len(),
        );
        let edge_resp = edge_builder
            .body(edgezero_core::body::Body::from(body_bytes))
            .change_context(PlatformError::HttpClient)?;

        Ok(PlatformResponse::new(edge_resp).with_backend_name(request.backend_name))
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl PlatformHttpClient for CloudflareHttpClient {
    async fn send(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        self.execute(request).await
    }

    fn supports_concurrent_fanout(&self) -> bool {
        // `send_async` executes each request eagerly, so multiple pending
        // requests run sequentially. The auction orchestrator checks this
        // before launching more than one provider request.
        false
    }

    async fn send_async(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
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
            // execute() always buffers via resp.bytes().await → Body::Once.
            // Return a typed error rather than panicking in the request path
            // in case that edgezero implementation detail ever changes.
            edgezero_core::body::Body::Stream(_) => {
                return Err(Report::new(PlatformError::HttpClient).attach(
                    "unexpected streaming body from CloudflareHttpClient::execute \
                     — expected a buffered Body::Once",
                ));
            }
        };

        let pending = CloudflarePendingResponse {
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

        reject_multi_provider_fanout(pending_requests.len())?;

        let ready_platform = pending_requests.remove(0);
        let pending = ready_platform
            .downcast::<CloudflarePendingResponse>()
            .map_err(|_| {
                Report::new(PlatformError::HttpClient)
                    .attach("unexpected inner type in CloudflareHttpClient::select")
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
            failed_backend_name: None,
        })
    }
}

// ---------------------------------------------------------------------------
// CloudflareSecretStoreAdapter — WASM target only
//
// Secrets are the one platform surface that cannot be bridged through an
// edgezero handle: `SecretHandle::get_bytes` is async, but
// `PlatformSecretStore::get_bytes` is sync. The Cloudflare `env.secret()`
// call IS synchronous at the JS level, so we call it directly here.
// ---------------------------------------------------------------------------

/// Bridges [`worker::Env`] secrets to [`PlatformSecretStore`] by calling
/// `env.secret(key)` synchronously. Writes and deletes return errors.
#[cfg(target_arch = "wasm32")]
struct CloudflareSecretStoreAdapter {
    env: worker::Env,
}

#[cfg(target_arch = "wasm32")]
impl PlatformSecretStore for CloudflareSecretStoreAdapter {
    fn get_bytes(
        &self,
        _store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        match self.env.secret(key) {
            // worker 0.7: Secret implements Display via JsValue::as_string() which
            // returns the raw JS string value with no wrapping or debug formatting.
            // Verified in worker-rs src/env.rs: `impl Display for Secret { fn fmt ->
            // write!(f, "{}", self.inner.as_string().unwrap_or_default()) }`.
            Ok(secret) => Ok(secret.to_string().into_bytes()),
            Err(err) => Err(Report::new(PlatformError::SecretStore)
                .attach(format!("secret lookup failed for key `{key}`: {err}"))),
        }
    }

    fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore)
            .attach("secret store writes are not supported on Cloudflare Workers"))
    }

    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore)
            .attach("secret store writes are not supported on Cloudflare Workers"))
    }
}

#[cfg(target_arch = "wasm32")]
impl PlatformSecretWriter for CloudflareSecretStoreAdapter {
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
// build_runtime_services
// ---------------------------------------------------------------------------

/// Construct [`RuntimeServices`] for an incoming Cloudflare Workers request.
///
/// Config and KV are sourced from the edgezero handles that `run_app` injects
/// before routing — via the `TRUSTED_SERVER_CONFIG` env-var binding and the
/// `TRUSTED_SERVER_KV` KV namespace declared in `cloudflare.toml`. No
/// platform-specific `#[cfg]` is required for these two stores.
///
/// Secrets still require direct `worker::Env` access because
/// `SecretHandle::get_bytes` is async while `PlatformSecretStore::get_bytes`
/// is sync; the underlying `env.secret()` call is synchronous at the JS level.
///
/// Geo information is read from Cloudflare's injected request headers
/// (`cf-ipcountry`, etc.) which are present on all plans; headers absent on
/// the native host target simply produce empty/zero defaults.
pub fn build_runtime_services(ctx: &edgezero_core::context::RequestContext) -> RuntimeServices {
    let client_ip = extract_client_ip(ctx);

    #[cfg(target_arch = "wasm32")]
    let http_client: Arc<dyn PlatformHttpClient> = Arc::new(CloudflareHttpClient);
    #[cfg(not(target_arch = "wasm32"))]
    let http_client: Arc<dyn PlatformHttpClient> = Arc::new(UnavailableHttpClient);

    // Config reads resolve through the whole ConfigRegistry (from request
    // extensions) so non-default logical ids (e.g. `jwks_store`) resolve; writes
    // delegate to the run_app-injected default config handle. An absent registry
    // makes composite reads error rather than silently reading a default store.
    let config_reader = ctx.request().extensions().get::<ConfigRegistry>().cloned();
    let config_writer: Arc<dyn PlatformConfigWriter> = ctx
        .config_store_default()
        .map(|h| Arc::new(ConfigStoreHandleAdapter(h)) as Arc<dyn PlatformConfigWriter>)
        .unwrap_or_else(|| Arc::new(NoopConfigStore));
    let config_store: Arc<dyn PlatformConfigStore> =
        Arc::new(CompositeConfigStore::new(config_reader, config_writer));

    // KV: the default request-path store stays the run_app-injected handle; the
    // full KvRegistry is carried separately for `kv_handle_named` lookups.
    let kv_store: Arc<dyn PlatformKvStore> = ctx
        .kv_store_default()
        .map(|h| Arc::new(KvHandleAdapter(h)) as Arc<dyn PlatformKvStore>)
        .unwrap_or_else(|| Arc::new(UnavailableKvStore));
    let kv_registry = ctx.request().extensions().get::<KvRegistry>().cloned();

    // Secret reads resolve through the whole SecretRegistry (from request
    // extensions); writes delegate to the wasm32 env.secret()-backed writer.
    let secret_reader = ctx.request().extensions().get::<SecretRegistry>().cloned();
    #[cfg(target_arch = "wasm32")]
    let secret_writer: Arc<dyn PlatformSecretWriter> =
        edgezero_adapter_cloudflare::context::CloudflareRequestContext::get(ctx.request())
            .map(|cf_ctx| {
                Arc::new(CloudflareSecretStoreAdapter {
                    env: cf_ctx.env().clone(),
                }) as Arc<dyn PlatformSecretWriter>
            })
            .unwrap_or_else(|| Arc::new(NoopSecretStore));
    #[cfg(not(target_arch = "wasm32"))]
    let secret_writer: Arc<dyn PlatformSecretWriter> = Arc::new(NoopSecretStore);
    let secret_store: Arc<dyn PlatformSecretStore> =
        Arc::new(CompositeSecretStore::new(secret_reader, secret_writer));

    // Geo: read Cloudflare-injected headers — no #[cfg] needed; headers are
    // simply absent on the native host target, producing Ok(None) from lookup().
    let geo = build_geo(ctx);

    RuntimeServices::builder()
        .config_store(config_store)
        .secret_store(secret_store)
        .kv_store(kv_store)
        .kv_registry(kv_registry)
        .backend(Arc::new(NoopBackend))
        .http_client(http_client)
        .geo(Arc::new(geo))
        .client_info(ClientInfo {
            client_ip,
            tls_protocol: None,
            tls_cipher: None,
            ..ClientInfo::default()
        })
        .build()
}

// ---------------------------------------------------------------------------
// Geo — reads Cloudflare-injected request headers (no #[cfg] needed)
// ---------------------------------------------------------------------------

/// Reads Cloudflare geo headers injected by the Workers runtime.
///
/// `cf-ipcountry` is available on all plans. `cf-ipcity`, `cf-ipcontinent`,
/// `cf-iplatitude`, and `cf-iplongitude` require an Enterprise plan. Absent or
/// unparseable values default to empty strings or `0.0`. Country code `XX`
/// (Cloudflare's "unknown" sentinel) is treated as absent.
struct CloudflareGeo {
    country: String,
    city: String,
    continent: String,
    latitude: f64,
    longitude: f64,
}

impl PlatformGeo for CloudflareGeo {
    fn lookup(&self, _client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        if self.country.is_empty() {
            return Ok(None);
        }
        Ok(Some(GeoInfo {
            city: self.city.clone(),
            country: self.country.clone(),
            continent: self.continent.clone(),
            latitude: self.latitude,
            longitude: self.longitude,
            metro_code: 0,
            region: None,
            asn: None,
        }))
    }
}

fn build_geo(ctx: &edgezero_core::context::RequestContext) -> CloudflareGeo {
    let headers = ctx.request().headers();
    let country = headers
        .get("cf-ipcountry")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty() && *s != "XX")
        .unwrap_or("")
        .to_string();
    let city = headers
        .get("cf-ipcity")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let continent = headers
        .get("cf-ipcontinent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let latitude = headers
        .get("cf-iplatitude")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    let longitude = headers
        .get("cf-iplongitude")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    CloudflareGeo {
        country,
        city,
        continent,
        latitude,
        longitude,
    }
}

fn extract_client_ip(ctx: &edgezero_core::context::RequestContext) -> Option<IpAddr> {
    ctx.request()
        .headers()
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
}

/// Reject multi-provider auction fan-out at the Cloudflare adapter level.
///
/// Cloudflare Workers executes `send_async` eagerly (no true concurrency), so
/// N simultaneous DSP requests run sequentially and accrue `sum(latencies)`
/// instead of `max(latencies)`.
///
/// The primary guard lives in the auction orchestrator, which checks
/// `supports_concurrent_fanout()` and rejects multi-provider configurations
/// before any request launches. This `select`-time rejection is
/// defense-in-depth for callers that bypass that check.
///
/// Extracted as a free function so the critical control-flow is testable on
/// native targets where the `#[cfg(target_arch = "wasm32")]` `select` impl
/// is excluded from the test binary.
#[cfg(any(target_arch = "wasm32", test))]
fn reject_multi_provider_fanout(len: usize) -> Result<(), Report<PlatformError>> {
    if len >= 2 {
        return Err(Report::new(PlatformError::HttpClient).attach(format!(
            "CloudflareHttpClient: multi-provider fan-out is not supported \
             ({len} providers submitted). Configure a single auction provider \
             or use the Fastly adapter for parallel DSP fan-out."
        )));
    }
    Ok(())
}

#[cfg(test)]
mod registry_test_support {
    //! In-memory store doubles and a `RequestContext` builder that seeds the
    //! three `EdgeZero` registries into request extensions, so adapter tests can
    //! exercise non-default logical store resolution through the composite.

    use std::collections::{BTreeMap, HashMap};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use bytes::Bytes;
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
    /// extensions, mirroring what `run_app` does before routing.
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
    use edgezero_core::context::RequestContext;
    use edgezero_core::http::{HeaderValue, request_builder};
    use edgezero_core::params::PathParams;

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

        let jwk = services
            .config_store()
            .get(&StoreName::from("jwks_store"), "kid-1")
            .expect("should resolve the non-default jwks_store through the composite");
        assert_eq!(
            jwk, "{\"kty\":\"OKP\"}",
            "should read the seeded value from the non-default config store"
        );

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
        // Both ids carry the same key with different values, so a named lookup
        // that resolved to the default store would be caught.
        let kv = kv_registry(
            &[
                ("trusted_server_kv", "marker", b"default-value"),
                ("consent_store", "marker", b"consent-value"),
            ],
            "trusted_server_kv",
        );
        let ctx = test_context_with_registries(None, Some(kv), None);
        let services = build_runtime_services(&ctx);

        let named = services
            .kv_handle_named("consent_store")
            .expect("should resolve the consent_store handle");
        let named_value = named
            .get_bytes("marker")
            .await
            .expect("should read from consent_store")
            .expect("should find the seeded key");
        assert_eq!(
            named_value.as_ref(),
            b"consent-value",
            "named lookup should read consent_store, not the default store"
        );

        let default_value = services
            .kv_handle()
            .get_bytes("marker")
            .await
            .expect("should read from the default KV store")
            .expect("should find the seeded key");
        assert_eq!(
            default_value.as_ref(),
            b"default-value",
            "the default request-path KV store must stay distinct from consent_store"
        );

        assert!(
            services.kv_handle_named("no_such_store").is_none(),
            "unknown KV id should resolve to None"
        );
    }

    fn make_ctx_with_header(name: &str, value: &str) -> RequestContext {
        let req = request_builder()
            .method("GET")
            .uri("https://example.com/")
            .header(
                name,
                HeaderValue::from_str(value).expect("should parse test header value"),
            )
            .body(edgezero_core::body::Body::empty())
            .expect("should build test request");
        RequestContext::new(req, PathParams::default())
    }

    fn make_ctx_without_header() -> RequestContext {
        let req = request_builder()
            .method("GET")
            .uri("https://example.com/")
            .body(edgezero_core::body::Body::empty())
            .expect("should build test request");
        RequestContext::new(req, PathParams::default())
    }

    #[test]
    fn extract_client_ip_parses_cf_connecting_ip() {
        let ctx = make_ctx_with_header("cf-connecting-ip", "203.0.113.42");
        let ip = extract_client_ip(&ctx);
        assert_eq!(
            ip,
            Some("203.0.113.42".parse().expect("should parse test IP")),
            "should parse cf-connecting-ip header"
        );
    }

    #[test]
    fn extract_client_ip_returns_none_when_header_absent() {
        let ctx = make_ctx_without_header();
        assert!(
            extract_client_ip(&ctx).is_none(),
            "should return None when cf-connecting-ip is not set"
        );
    }

    #[test]
    fn extract_client_ip_returns_none_for_invalid_ip() {
        let ctx = make_ctx_with_header("cf-connecting-ip", "not-an-ip");
        assert!(
            extract_client_ip(&ctx).is_none(),
            "should return None for an unparseable IP string"
        );
    }

    #[test]
    fn build_geo_returns_country_from_header() {
        let ctx = make_ctx_with_header("cf-ipcountry", "US");
        let geo = build_geo(&ctx);
        assert_eq!(geo.country, "US", "should extract cf-ipcountry");
    }

    #[test]
    fn build_geo_treats_xx_as_absent() {
        let ctx = make_ctx_with_header("cf-ipcountry", "XX");
        let geo = build_geo(&ctx);
        assert!(geo.country.is_empty(), "XX should be treated as absent");
    }

    #[test]
    fn build_geo_lookup_returns_none_when_country_absent() {
        let ctx = make_ctx_without_header();
        let geo = build_geo(&ctx);
        assert!(
            geo.lookup(None)
                .expect("should perform geo lookup")
                .is_none(),
            "should return None when no country header"
        );
    }

    #[test]
    fn build_geo_lookup_returns_some_with_populated_country() {
        let req = request_builder()
            .method("GET")
            .uri("https://example.com/")
            .header("cf-ipcountry", HeaderValue::from_static("US"))
            .header("cf-ipcity", HeaderValue::from_static("New York"))
            .header("cf-ipcontinent", HeaderValue::from_static("NA"))
            .header("cf-iplatitude", HeaderValue::from_static("40.71"))
            .header("cf-iplongitude", HeaderValue::from_static("-74.01"))
            .body(edgezero_core::body::Body::empty())
            .expect("should build test request");
        let ctx = RequestContext::new(req, PathParams::default());
        let geo = build_geo(&ctx);
        let info = geo
            .lookup(None)
            .expect("should perform geo lookup")
            .expect("should return GeoInfo when country is set");
        assert_eq!(info.country, "US", "should populate country");
        assert_eq!(info.city, "New York", "should populate city");
        assert_eq!(info.continent, "NA", "should populate continent");
        assert!(
            (info.latitude - 40.71).abs() < 0.01,
            "should populate latitude"
        );
        assert!(
            (info.longitude - (-74.01)).abs() < 0.01,
            "should populate longitude"
        );
    }

    // ---------------------------------------------------------------------------
    // reject_multi_provider_fanout tests
    // ---------------------------------------------------------------------------

    #[test]
    fn reject_multi_provider_fanout_passes_empty() {
        assert!(
            reject_multi_provider_fanout(0).is_ok(),
            "len=0 should pass (empty list caught separately in select)"
        );
    }

    #[test]
    fn reject_multi_provider_fanout_passes_single_provider() {
        assert!(
            reject_multi_provider_fanout(1).is_ok(),
            "single provider should be allowed"
        );
    }

    #[test]
    fn reject_multi_provider_fanout_rejects_two_providers() {
        assert!(
            reject_multi_provider_fanout(2).is_err(),
            "len=2 should be rejected"
        );
    }

    #[test]
    fn reject_multi_provider_fanout_rejects_five_providers() {
        let err = reject_multi_provider_fanout(5).expect_err("should reject five providers");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("5"),
            "error message should include provider count"
        );
    }
}
