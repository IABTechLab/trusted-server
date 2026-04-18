use std::net::IpAddr;
use std::sync::Arc;

use error_stack::Report;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore, PlatformError,
    PlatformGeo, PlatformHttpClient, PlatformSecretStore, RuntimeServices, StoreId, StoreName,
    UnavailableKvStore,
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
// Noop stubs for host-target builds (native CI, unit tests)
// ---------------------------------------------------------------------------

// TODO: wire edgezero-adapter-cloudflare's CloudflareConfigStore / CloudflareSecretStore
// for the wasm32 path so that key rotation (/admin/keys/rotate|deactivate) and
// signing config work on deployed Workers. Until then, all config/secret reads
// return errors on both native and WASM32 targets.
struct NoopConfigStore;

impl PlatformConfigStore for NoopConfigStore {
    fn get(&self, _: &StoreName, _: &str) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("unavailable on host target"))
    }

    fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("unavailable on host target"))
    }

    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("unavailable on host target"))
    }
}

struct NoopSecretStore;

impl PlatformSecretStore for NoopSecretStore {
    fn get_bytes(&self, _: &StoreName, _: &str) -> Result<Vec<u8>, Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore).attach("unavailable on host target"))
    }

    fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore).attach("unavailable on host target"))
    }

    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore).attach("unavailable on host target"))
    }
}

struct NoopBackend;

impl PlatformBackend for NoopBackend {
    fn predict_name(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Ok(format!("{}_{}", spec.scheme, spec.host))
    }

    fn ensure(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        self.predict_name(spec)
    }
}

struct NoopGeo;

impl PlatformGeo for NoopGeo {
    fn lookup(&self, _: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
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
/// `send_async` eagerly awaits (sequential fan-out) — acceptable for Workers
/// since Cloudflare's own runtime handles true parallelism at the event level.
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
// build_runtime_services
// ---------------------------------------------------------------------------

/// Construct [`RuntimeServices`] for an incoming Cloudflare Workers request.
///
/// On native (host target, CI), the HTTP client degrades to [`UnavailableHttpClient`]
/// since `worker::Fetch` is only available on `wasm32`. On the Workers runtime the
/// real [`CloudflareHttpClient`] is used so outbound proxy requests succeed.
pub fn build_runtime_services(ctx: &edgezero_core::context::RequestContext) -> RuntimeServices {
    let client_ip = extract_client_ip(ctx);

    #[cfg(target_arch = "wasm32")]
    let http_client: Arc<dyn PlatformHttpClient> = Arc::new(CloudflareHttpClient);
    #[cfg(not(target_arch = "wasm32"))]
    let http_client: Arc<dyn PlatformHttpClient> = Arc::new(UnavailableHttpClient);

    RuntimeServices::builder()
        .config_store(Arc::new(NoopConfigStore))
        .secret_store(Arc::new(NoopSecretStore))
        .kv_store(Arc::new(UnavailableKvStore))
        .backend(Arc::new(NoopBackend))
        .http_client(http_client)
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo {
            client_ip,
            tls_protocol: None,
            tls_cipher: None,
        })
        .build()
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
}
