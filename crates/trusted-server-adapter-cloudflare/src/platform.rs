use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use edgezero_core::{ConfigStoreHandle, KvHandle, KvPage, KvStore};
use error_stack::Report;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, KvError, PlatformBackend, PlatformBackendSpec, PlatformConfigStore,
    PlatformError, PlatformGeo, PlatformHttpClient, PlatformKvStore, PlatformSecretStore,
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
        self.0
            .get(key)
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
/// # Sequential fan-out and auction latency
///
/// `send_async` eagerly awaits each request before returning, so parallel
/// fan-out (e.g. auction DSP calls) becomes sequential: total latency is
/// `sum(DSP_i)` rather than `max(DSP_i)`. With a 300 ms auction budget and
/// three DSPs averaging 80 ms each, all three return in time; with five DSPs
/// the last two may be cut off by the orchestrator's remaining-budget check.
///
/// Cloudflare Workers does support concurrent `fetch` calls via `Promise.all`,
/// but the `?Send` bound on `PlatformHttpClient` prevents using `join!` across
/// requests here. A future revision could implement true fan-out by spawning all
/// futures inside `select` before polling, at the cost of a more complex
/// implementation.
///
/// Individual fetch calls have no explicit timeout. The Workers runtime enforces
/// a global CPU time limit per invocation (default 30 s wall-clock on paid plans)
/// which acts as an implicit upper bound.
#[cfg(target_arch = "wasm32")]
pub struct CloudflareHttpClient;

#[cfg(target_arch = "wasm32")]
impl CloudflareHttpClient {
    async fn execute(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        use worker::{Fetch, Headers, Method, Request, RequestInit};

        let uri = request.request.uri().to_string();
        let method = Method::from(request.request.method().as_str().to_ascii_uppercase());

        let headers = Headers::new();
        for (name, value) in request.request.headers() {
            headers
                .set(name.as_str(), &String::from_utf8_lossy(value.as_bytes()))
                .change_context(PlatformError::HttpClient)?;
        }

        let (_, body) = request.request.into_parts();
        let body_bytes = match body {
            edgezero_core::body::Body::Once(bytes) => bytes.to_vec(),
            edgezero_core::body::Body::Stream(_) => {
                log::warn!(
                    "CloudflareHttpClient: Body::Stream is not supported; \
                     outbound request body will be empty"
                );
                vec![]
            }
        };

        let mut init = RequestInit::new();
        init.with_method(method).with_headers(headers);
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
        let mut edge_builder = edgezero_core::http::response_builder().status(status);
        for (name, value) in resp.headers().entries() {
            // The Workers runtime auto-decompresses gzip/br/deflate and handles
            // chunked transfer — strip these headers so the proxy layer does not
            // attempt a second decompression pass on the already-decoded body.
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "content-encoding" | "transfer-encoding"
            ) {
                continue;
            }
            edge_builder = edge_builder.header(name.as_str(), value.as_bytes());
        }
        let body_bytes = resp
            .bytes()
            .await
            .change_context(PlatformError::HttpClient)?;
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
            edgezero_core::body::Body::Stream(_) => vec![],
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

        // Warn when the orchestrator submitted more than one provider: send_async()
        // executes eagerly, so those requests already ran sequentially rather than
        // in parallel. Total auction latency is sum(DSP_i) instead of max(DSP_i).
        if pending_requests.len() >= 2 {
            log::warn!(
                "CloudflareHttpClient: select() received {} pending requests; \
                 send_async() runs each request eagerly so multi-provider auctions \
                 degrade to sequential latency. Use the Fastly adapter for parallel \
                 DSP fan-out.",
                pending_requests.len()
            );
        }

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

    // Config: use the ConfigStoreHandle injected by run_app — no #[cfg] needed.
    let config_store: Arc<dyn PlatformConfigStore> = ctx
        .config_store()
        .map(|h| Arc::new(ConfigStoreHandleAdapter(h)) as Arc<dyn PlatformConfigStore>)
        .unwrap_or_else(|| Arc::new(NoopConfigStore));

    // KV: use the KvHandle injected by run_app — no #[cfg] needed.
    let kv_store: Arc<dyn PlatformKvStore> = ctx
        .kv_handle()
        .map(|h| Arc::new(KvHandleAdapter(h)) as Arc<dyn PlatformKvStore>)
        .unwrap_or_else(|| Arc::new(UnavailableKvStore));

    // Secrets: still requires wasm32-specific env.secret() (async/sync mismatch).
    #[cfg(target_arch = "wasm32")]
    let secret_store: Arc<dyn PlatformSecretStore> =
        edgezero_adapter_cloudflare::CloudflareRequestContext::get(ctx.request())
            .map(|cf_ctx| {
                Arc::new(CloudflareSecretStoreAdapter {
                    env: cf_ctx.env().clone(),
                }) as Arc<dyn PlatformSecretStore>
            })
            .unwrap_or_else(|| Arc::new(NoopSecretStore));
    #[cfg(not(target_arch = "wasm32"))]
    let secret_store: Arc<dyn PlatformSecretStore> = Arc::new(NoopSecretStore);

    // Geo: read Cloudflare-injected headers — no #[cfg] needed; headers are
    // simply absent on the native host target, producing Ok(None) from lookup().
    let geo = build_geo(ctx);

    RuntimeServices::builder()
        .config_store(config_store)
        .secret_store(secret_store)
        .kv_store(kv_store)
        .backend(Arc::new(NoopBackend))
        .http_client(http_client)
        .geo(Arc::new(geo))
        .client_info(ClientInfo {
            client_ip,
            tls_protocol: None,
            tls_cipher: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use edgezero_core::context::RequestContext;
    use edgezero_core::http::{HeaderValue, request_builder};
    use edgezero_core::params::PathParams;

    fn make_ctx_with_header(name: &str, value: &str) -> RequestContext {
        let req = request_builder()
            .method("GET")
            .uri("https://example.com/")
            .header(name, HeaderValue::from_str(value).unwrap())
            .body(edgezero_core::body::Body::empty())
            .unwrap();
        RequestContext::new(req, PathParams::default())
    }

    fn make_ctx_without_header() -> RequestContext {
        let req = request_builder()
            .method("GET")
            .uri("https://example.com/")
            .body(edgezero_core::body::Body::empty())
            .unwrap();
        RequestContext::new(req, PathParams::default())
    }

    #[test]
    fn extract_client_ip_parses_cf_connecting_ip() {
        let ctx = make_ctx_with_header("cf-connecting-ip", "203.0.113.42");
        let ip = extract_client_ip(&ctx);
        assert_eq!(
            ip,
            Some("203.0.113.42".parse().unwrap()),
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
            geo.lookup(None).unwrap().is_none(),
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
            .unwrap();
        let ctx = RequestContext::new(req, PathParams::default());
        let geo = build_geo(&ctx);
        let info = geo
            .lookup(None)
            .unwrap()
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
}
