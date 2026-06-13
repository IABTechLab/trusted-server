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

#[cfg(not(all(feature = "spin", target_arch = "wasm32")))]
use trusted_server_core::platform::UnavailableHttpClient;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use error_stack::ResultExt as _;
#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
use std::io::Read as _;
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use trusted_server_core::platform::{
    PlatformHttpRequest, PlatformPendingRequest, PlatformResponse, PlatformSelectResult,
};

// 8 MiB ceiling: conservative for ad-server responses while leaving headroom in
// the Spin WASM component heap. Larger responses from misbehaving origins are
// rejected with a typed error rather than OOMing the component.
#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
const MAX_DECOMPRESSED_SIZE: usize = 8 * 1024 * 1024;

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
type HeaderPairs = Vec<(String, Vec<u8>)>;
#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
type BufferedResponseParts = (HeaderPairs, Vec<u8>);

const SPIN_VARIABLE_HEX: &[u8; 16] = b"0123456789abcdef";

// ---------------------------------------------------------------------------
// Noop stubs — used when a handle is absent (native CI, missing binding)
// ---------------------------------------------------------------------------

struct NoopConfigStore;

impl PlatformConfigStore for NoopConfigStore {
    fn get(&self, _: &StoreName, _: &str) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("config store not available"))
    }

    fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("config store writes are not supported"))
    }

    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("config store writes are not supported"))
    }
}

#[cfg(not(all(feature = "spin", target_arch = "wasm32")))]
struct NoopSecretStore;

#[cfg(not(all(feature = "spin", target_arch = "wasm32")))]
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
// edgezero handle adapters — injected by edgezero_adapter_spin::run_app
// ---------------------------------------------------------------------------

/// Bridges edgezero's [`ConfigStoreHandle`] to [`PlatformConfigStore`].
///
/// Reads delegate through the handle after mapping Trusted Server keys to Spin
/// variable names. Writes are unsupported on current Spin runtime config and
/// return typed errors.
struct ConfigStoreHandleAdapter(ConfigStoreHandle);

impl PlatformConfigStore for ConfigStoreHandleAdapter {
    fn get(&self, _store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        let variable_name = spin_variable_name(key, PlatformError::ConfigStore)?;
        self.0
            .get(&variable_name)
            .map_err(|e| {
                Report::new(PlatformError::ConfigStore)
                    .attach(format!(
                        "config store lookup failed for key `{key}` as Spin variable `{variable_name}`: {e}"
                    ))
            })?
            .ok_or_else(|| {
                Report::new(PlatformError::ConfigStore).attach(format!(
                    "key `{key}` not found as Spin variable `{variable_name}`"
                ))
            })
    }

    fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore)
            .attach("config store writes are not supported on Spin"))
    }

    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore)
            .attach("config store writes are not supported on Spin"))
    }
}

fn spin_variable_name(
    key: &str,
    error_context: PlatformError,
) -> Result<String, Report<PlatformError>> {
    if key.is_empty() {
        return Err(Report::new(error_context).attach("Spin variable key must not be empty"));
    }

    // Spin requires each _-separated word to start with an ASCII letter.
    // Reject at the encoder boundary so no caller can accidentally produce aliasing
    // (e.g. "1foo" and "n1foo" both encoding to the same variable name).
    if !key.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(Report::new(error_context).attach(format!(
            "Spin variable key `{key}` must start with a lowercase ASCII letter"
        )));
    }

    // `v_` prefix + worst-case 4 bytes per char for escape sequences.
    let mut out = String::with_capacity(key.len() * 4 + 3);
    out.push_str("v_");

    for ch in key.chars() {
        match ch {
            'a'..='z' | '0'..='9' => out.push(ch),
            'A'..='Z' | '-' | '_' | '.' | ':' => {
                push_spin_variable_escape(&mut out, ch as u8);
            }
            _ => {
                return Err(Report::new(error_context).attach(format!(
                    "Spin variable key `{key}` contains unsupported character `{ch}`"
                )));
            }
        }
    }

    Ok(out)
}

fn push_spin_variable_escape(out: &mut String, byte: u8) {
    out.push('_');
    out.push('x');
    out.push(SPIN_VARIABLE_HEX[(byte >> 4) as usize] as char);
    out.push(SPIN_VARIABLE_HEX[(byte & 0x0f) as usize] as char);
}

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
fn spin_secret_variable_name(
    store_name: &StoreName,
    key: &str,
) -> Result<String, Report<PlatformError>> {
    let store_variable = spin_variable_name(store_name.as_ref(), PlatformError::SecretStore)?;
    let key_variable = spin_variable_name(key, PlatformError::SecretStore)?;

    Ok(format!("{store_variable}_{key_variable}"))
}

/// Bridges edgezero's [`KvHandle`] to [`PlatformKvStore`].
///
/// Delegates all operations through `KvHandle`'s raw-bytes API. Spin KV TTL
/// support is determined by the underlying `EdgeZero` Spin KV implementation.
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
// Response policy helpers
// ---------------------------------------------------------------------------

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
fn sanitize_response_headers(headers: HeaderPairs) -> HeaderPairs {
    let connection_tokens = connection_header_tokens(&headers);

    headers
        .into_iter()
        .filter(|(name, _)| !is_hop_by_hop_response_header(name, &connection_tokens))
        .collect()
}

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
fn connection_header_tokens(headers: &HeaderPairs) -> Vec<String> {
    headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("connection"))
        .filter_map(|(_, value)| std::str::from_utf8(value).ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
fn is_hop_by_hop_response_header(name: &str, connection_tokens: &[String]) -> bool {
    let lowercase_name = name.to_ascii_lowercase();
    matches!(
        lowercase_name.as_str(),
        "connection"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "keep-alive"
    ) || connection_tokens
        .iter()
        .any(|token| token == &lowercase_name)
}

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
fn content_encoding(headers: &HeaderPairs) -> Option<String> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-encoding"))
        .and_then(|(_, value)| std::str::from_utf8(value).ok())
        .map(str::trim)
        .map(str::to_ascii_lowercase)
}

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
fn apply_spin_response_policy(
    headers: HeaderPairs,
    body: Vec<u8>,
) -> Result<BufferedResponseParts, Report<PlatformError>> {
    let mut headers = sanitize_response_headers(headers);
    let Some(encoding) = content_encoding(&headers) else {
        return Ok((headers, body));
    };

    if !matches!(encoding.as_str(), "gzip" | "br") {
        return Ok((headers, body));
    }

    let body = decompress_body(&body, &encoding, MAX_DECOMPRESSED_SIZE)?;
    headers.retain(|(name, _)| {
        !name.eq_ignore_ascii_case("content-encoding")
            && !name.eq_ignore_ascii_case("content-length")
    });
    Ok((headers, body))
}

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
fn decompress_body(
    body: &[u8],
    encoding: &str,
    max_size: usize,
) -> Result<Vec<u8>, Report<PlatformError>> {
    match encoding {
        "gzip" => {
            let decoder = flate2::read::GzDecoder::new(body);
            read_decompressed(decoder, encoding, max_size)
        }
        "br" => {
            let decoder = brotli::Decompressor::new(body, 8192);
            read_decompressed(decoder, encoding, max_size)
        }
        _ => Ok(body.to_vec()),
    }
}

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
fn read_decompressed<R>(
    reader: R,
    encoding: &str,
    max_size: usize,
) -> Result<Vec<u8>, Report<PlatformError>>
where
    R: std::io::Read,
{
    let mut decoded = Vec::new();
    reader
        .take(max_size as u64 + 1)
        .read_to_end(&mut decoded)
        .map_err(|e| {
            Report::new(PlatformError::HttpClient)
                .attach(format!("{encoding} decompression failed: {e}"))
        })?;
    if decoded.len() > max_size {
        return Err(Report::new(PlatformError::HttpClient).attach(format!(
            "{encoding} decompression exceeded maximum size of {max_size} bytes"
        )));
    }
    Ok(decoded)
}

// ---------------------------------------------------------------------------
// SpinPlatformHttpClient — WASM target + spin feature only
// ---------------------------------------------------------------------------

/// Returns `true` for headers forbidden on WASI HTTP outbound requests.
///
/// WASI HTTP (wasi:http@0.2) forbids `host` on outgoing requests — the
/// authority is conveyed separately via `OutgoingRequest::set_authority`.
/// Hop-by-hop headers (`connection`, `keep-alive`, `transfer-encoding`,
/// `upgrade`, `proxy-connection`) are HTTP/1.1 transport-layer concerns
/// that must not appear in WASI HTTP requests.
// Compiled for tests on native targets so unit tests can exercise the filter
// without requiring wasm32; the only production call site is inside the
// `#[cfg(all(feature = "spin", target_arch = "wasm32"))]` block below.
#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
fn is_wasi_forbidden_outbound_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("host")
        || name.eq_ignore_ascii_case("connection")
        || name.eq_ignore_ascii_case("keep-alive")
        || name.eq_ignore_ascii_case("transfer-encoding")
        || name.eq_ignore_ascii_case("upgrade")
        || name.eq_ignore_ascii_case("proxy-connection")
}

/// Carries a completed response through `send_async` → `select`.
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
struct SpinPendingResponse {
    backend_name: String,
    status: u16,
    headers: HeaderPairs,
    body: Vec<u8>,
}

/// `spin_sdk::http::send`-backed HTTP client for the Spin runtime.
///
/// `send_async` eagerly executes each request before returning, so
/// [`PlatformHttpClient::supports_concurrent_fanout`] reports `false` and the
/// auction orchestrator rejects multi-provider configurations before any
/// request launches. `select` keeps a defense-in-depth rejection for more
/// than one pending request, matching the Cloudflare adapter behavior.
///
/// # Known MVP limits
///
/// **No configurable outbound timeout.** `spin_sdk::http::send` does not
/// expose per-request timeout control, and [`PlatformBackendSpec::first_byte_timeout`]
/// is ignored by [`NoopBackend`]. A slow or hung origin will block the Spin
/// invocation for whatever default the Spin runtime imposes. Operators requiring
/// deterministic timeout behaviour should use the Fastly adapter.
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
pub struct SpinPlatformHttpClient;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
impl SpinPlatformHttpClient {
    async fn execute(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        let uri = request.request.uri().to_string();

        let mut builder = spin_sdk::http::Request::builder();
        builder
            .method(into_spin_method(request.request.method()))
            .uri(uri.clone());

        for (name, value) in request.request.headers() {
            // WASI HTTP forbids these headers on outbound requests:
            // `host` is conveyed via set_authority; hop-by-hop headers are HTTP/1.1 only.
            if is_wasi_forbidden_outbound_header(name.as_str()) {
                continue;
            }
            match value.to_str() {
                Ok(value) => {
                    builder.header(name.as_str(), value);
                }
                Err(_) => {
                    log::warn!(
                        "dropping non-UTF-8 outbound request header (Spin WASI limitation): {}",
                        name
                    );
                }
            }
        }

        let (_, body) = request.request.into_parts();
        let body_bytes = match body {
            edgezero_core::body::Body::Once(bytes) => bytes.to_vec(),
            edgezero_core::body::Body::Stream(_) => {
                // TODO: streaming request bodies unsupported; large proxy POBs (e.g.
                // /first-party/proxy-rebuild) will fail until Spin WASI HTTP buffering
                // is added to this client.
                return Err(Report::new(PlatformError::HttpClient)
                    .attach("streaming request bodies are not supported on Spin outbound HTTP"));
            }
        };
        builder.body(body_bytes);
        let spin_request = builder.build();

        let spin_response: spin_sdk::http::Response =
            spin_sdk::http::send(spin_request).await.map_err(|e| {
                Report::new(PlatformError::HttpClient)
                    .attach(format!("outbound request to {uri} failed: {e}"))
            })?;

        let status = *spin_response.status();
        let headers: HeaderPairs = spin_response
            .headers()
            .map(|(name, value)| (name.to_string(), value.as_bytes().to_vec()))
            .collect();
        let body = spin_response.into_body();

        let (headers, body) = apply_spin_response_policy(headers, body)?;
        let mut edge_builder = edgezero_core::http::response_builder().status(status);
        for (name, value) in headers {
            edge_builder = edge_builder.header(name.as_str(), value.as_slice());
        }
        let edge_resp = edge_builder
            .body(edgezero_core::body::Body::from(body))
            .change_context(PlatformError::HttpClient)?;

        Ok(PlatformResponse::new(edge_resp).with_backend_name(request.backend_name))
    }
}

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
#[async_trait::async_trait(?Send)]
impl PlatformHttpClient for SpinPlatformHttpClient {
    fn supports_concurrent_fanout(&self) -> bool {
        // `send_async` executes each request eagerly, so multiple pending
        // requests run sequentially. The auction orchestrator checks this
        // before launching more than one provider request.
        false
    }

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
        let headers: HeaderPairs = response
            .response
            .headers()
            .iter()
            .map(|(n, v)| (n.to_string(), v.as_bytes().to_vec()))
            .collect();
        let body_bytes = match response.response.into_body() {
            edgezero_core::body::Body::Once(bytes) => bytes.to_vec(),
            edgezero_core::body::Body::Stream(_) => {
                return Err(Report::new(PlatformError::HttpClient)
                    .attach("SpinPlatformHttpClient::execute returned a streaming body"));
            }
        };

        let pending = SpinPendingResponse {
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

        if pending_requests.len() >= 2 {
            return Err(Report::new(PlatformError::HttpClient).attach(format!(
                "SpinPlatformHttpClient: multi-provider fan-out is not supported \
                 ({} providers submitted). Configure a single auction provider \
                 or use the Fastly adapter for parallel DSP fan-out.",
                pending_requests.len()
            )));
        }

        let ready_platform = pending_requests.remove(0);
        let pending = ready_platform
            .downcast::<SpinPendingResponse>()
            .map_err(|_| {
                Report::new(PlatformError::HttpClient)
                    .attach("unexpected inner type in SpinPlatformHttpClient::select")
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

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
fn into_spin_method(method: &edgezero_core::http::Method) -> spin_sdk::http::Method {
    match *method {
        edgezero_core::http::Method::GET => spin_sdk::http::Method::Get,
        edgezero_core::http::Method::POST => spin_sdk::http::Method::Post,
        edgezero_core::http::Method::PUT => spin_sdk::http::Method::Put,
        edgezero_core::http::Method::DELETE => spin_sdk::http::Method::Delete,
        edgezero_core::http::Method::PATCH => spin_sdk::http::Method::Patch,
        edgezero_core::http::Method::HEAD => spin_sdk::http::Method::Head,
        edgezero_core::http::Method::OPTIONS => spin_sdk::http::Method::Options,
        edgezero_core::http::Method::CONNECT => spin_sdk::http::Method::Connect,
        edgezero_core::http::Method::TRACE => spin_sdk::http::Method::Trace,
        ref other => spin_sdk::http::Method::Other(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// SpinSecretStoreAdapter — WASM target + spin feature only
// ---------------------------------------------------------------------------

/// Bridges Spin component variables to [`PlatformSecretStore`].
///
/// # Limitations
///
/// - **UTF-8 only.** `spin_sdk::variables::get` returns a `String`, so all
///   secret values must be valid UTF-8. JSON-encoded signing keys work today;
///   raw binary secrets (e.g. bare Ed25519 bytes) would silently fail at the
///   Spin runtime layer.
///
/// - **Plaintext at rest.** Spin component variables are unencrypted in the
///   application manifest by default. Production deployments must back variables
///   with a real secret-provider source (e.g. Vault, Azure Key Vault) to avoid
///   storing signing keys in plaintext on disk.
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
struct SpinSecretStoreAdapter;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
impl PlatformSecretStore for SpinSecretStoreAdapter {
    fn get_bytes(
        &self,
        store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        let variable_name = spin_secret_variable_name(store_name, key)?;
        match spin_sdk::variables::get(&variable_name) {
            Ok(value) => Ok(value.into_bytes()),
            Err(error) => Err(Report::new(PlatformError::SecretStore).attach(format!(
                "secret lookup failed for key `{key}` as Spin variable `{variable_name}`: {error}"
            ))),
        }
    }

    fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore)
            .attach("secret store writes are not supported on Spin"))
    }

    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore)
            .attach("secret store deletes are not supported on Spin"))
    }
}

// ---------------------------------------------------------------------------
// build_runtime_services
// ---------------------------------------------------------------------------

/// Construct [`RuntimeServices`] for an incoming Spin request.
///
/// Config and KV are sourced from the `EdgeZero` handles that `run_app` injects
/// before routing. Secrets are read synchronously from Spin component
/// variables because Trusted Server's platform secret trait is sync.
#[must_use]
pub fn build_runtime_services(ctx: &edgezero_core::context::RequestContext) -> RuntimeServices {
    let client_ip = extract_client_ip(ctx);

    #[cfg(all(feature = "spin", target_arch = "wasm32"))]
    let http_client: Arc<dyn PlatformHttpClient> = Arc::new(SpinPlatformHttpClient);
    #[cfg(not(all(feature = "spin", target_arch = "wasm32")))]
    let http_client: Arc<dyn PlatformHttpClient> = Arc::new(UnavailableHttpClient);

    let config_store: Arc<dyn PlatformConfigStore> = ctx
        .config_store()
        .map(|h| Arc::new(ConfigStoreHandleAdapter(h)) as Arc<dyn PlatformConfigStore>)
        .unwrap_or_else(|| Arc::new(NoopConfigStore));

    let kv_store: Arc<dyn PlatformKvStore> = ctx
        .kv_handle()
        .map(|h| Arc::new(KvHandleAdapter(h)) as Arc<dyn PlatformKvStore>)
        .unwrap_or_else(|| Arc::new(UnavailableKvStore));

    #[cfg(all(feature = "spin", target_arch = "wasm32"))]
    let secret_store: Arc<dyn PlatformSecretStore> = Arc::new(SpinSecretStoreAdapter);
    #[cfg(not(all(feature = "spin", target_arch = "wasm32")))]
    let secret_store: Arc<dyn PlatformSecretStore> = Arc::new(NoopSecretStore);

    RuntimeServices::builder()
        .config_store(config_store)
        .secret_store(secret_store)
        .kv_store(kv_store)
        .backend(Arc::new(NoopBackend))
        .http_client(http_client)
        .geo(Arc::new(NullGeo))
        .client_info(ClientInfo {
            client_ip,
            tls_protocol: None,
            tls_cipher: None,
        })
        .build()
}

// ---------------------------------------------------------------------------
// Geo and client info
// ---------------------------------------------------------------------------

struct NullGeo;

impl PlatformGeo for NullGeo {
    fn lookup(&self, _client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
    }
}

fn extract_client_ip(ctx: &edgezero_core::context::RequestContext) -> Option<IpAddr> {
    edgezero_adapter_spin::SpinRequestContext::get(ctx.request()).and_then(|c| c.client_addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    use edgezero_core::body::Body;
    use edgezero_core::context::RequestContext;
    use edgezero_core::http::request_builder;
    use edgezero_core::params::PathParams;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write as _;

    fn make_ctx_without_spin_context() -> RequestContext {
        let req = request_builder()
            .method("GET")
            .uri("https://example.com/")
            .body(Body::empty())
            .expect("should build request");
        RequestContext::new(req, PathParams::default())
    }

    fn gzip_bytes(input: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(input)
            .expect("should write gzip test payload");
        encoder.finish().expect("should finish gzip test payload")
    }

    fn brotli_bytes(input: &[u8]) -> Vec<u8> {
        let mut compressed = Vec::new();
        {
            let mut compressor = brotli::CompressorWriter::new(&mut compressed, 4096, 5, 21);
            compressor
                .write_all(input)
                .expect("should write brotli test payload");
        }
        compressed
    }

    #[test]
    fn extract_client_ip_reads_spin_request_context() {
        let mut req = request_builder()
            .method("GET")
            .uri("https://example.com/")
            .body(Body::empty())
            .expect("should build request");
        edgezero_adapter_spin::SpinRequestContext::insert(
            &mut req,
            edgezero_adapter_spin::SpinRequestContext {
                client_addr: Some("203.0.113.42".parse().expect("should parse test IP")),
                full_url: None,
            },
        );
        let ctx = RequestContext::new(req, PathParams::default());

        assert_eq!(
            extract_client_ip(&ctx),
            Some("203.0.113.42".parse().expect("should parse test IP")),
            "should read Spin client IP from request context"
        );
    }

    #[test]
    fn extract_client_ip_returns_none_without_spin_context() {
        let ctx = make_ctx_without_spin_context();

        assert!(
            extract_client_ip(&ctx).is_none(),
            "should return None when Spin context is absent"
        );
    }

    #[test]
    fn null_geo_always_returns_none() {
        let geo = NullGeo;

        assert!(
            geo.lookup(None).expect("should not fail").is_none(),
            "should return None without a client IP"
        );
        assert!(
            geo.lookup(Some("127.0.0.1".parse().expect("should parse IP")))
                .expect("should not fail")
                .is_none(),
            "should return None for any client IP"
        );
    }

    #[test]
    fn spin_variable_name_encodes_trusted_server_keys() {
        assert_eq!(
            spin_variable_name("current-kid", PlatformError::ConfigStore)
                .expect("should encode current kid key"),
            "v_current_x2dkid"
        );
        assert_eq!(
            spin_variable_name("active-kids", PlatformError::ConfigStore)
                .expect("should encode active kids key"),
            "v_active_x2dkids"
        );
        assert_eq!(
            spin_variable_name("ts-2026-05-25", PlatformError::ConfigStore)
                .expect("should encode generated kid"),
            "v_ts_x2d2026_x2d05_x2d25"
        );
        // Digit-leading keys are rejected at the encoder boundary.
        assert!(
            spin_variable_name("2026-key", PlatformError::ConfigStore).is_err(),
            "should reject digit-leading key"
        );
    }

    #[test]
    fn spin_encoder_accepts_every_creatable_kid() {
        // Portability contract: core's create/rotate validation (kid_is_creatable)
        // must never admit a kid the Spin variable encoder rejects — otherwise such
        // a kid would 400 on create across every adapter yet 5xx at storage on Spin.
        // This pins core >= encoder strictness so the duplicated lowercase-leading
        // rule in validate_kid and spin_variable_name cannot silently drift.
        use trusted_server_core::request_signing::kid_is_creatable;

        let samples = [
            "a",
            "kid",
            "ts-2026-05-25",
            "azAZ09-_.:",
            "k.id:with_all-chars",
            "KidA",
            "_kid",
            "-kid",
            ".kid",
            ":kid",
            "1foo",
            "0abc",
            "",
            "a,b",
            "a b",
            "kid/name",
        ];
        for kid in samples {
            if kid_is_creatable(kid) {
                assert!(
                    spin_variable_name(kid, PlatformError::ConfigStore).is_ok(),
                    "core accepts kid `{kid}` but the Spin encoder rejects it \
                     (portability contract broken)"
                );
            }
        }
    }

    #[test]
    fn spin_secret_variable_name_prefixes_store_name() {
        assert_eq!(
            spin_secret_variable_name(&StoreName::from("signing_keys"), "ts-2026-05-25")
                .expect("should encode secret variable name"),
            "v_signing_x5fkeys_v_ts_x2d2026_x2d05_x2d25"
        );
    }

    #[test]
    fn spin_variable_name_rejects_unsupported_characters() {
        assert!(
            spin_variable_name("kid/name", PlatformError::ConfigStore).is_err(),
            "should reject characters outside the supported Spin variable mapping"
        );
    }

    #[test]
    fn spin_variable_name_does_not_collapse_distinct_allowed_kids() {
        let mut names = std::collections::BTreeSet::new();
        // Only lowercase-leading keys are accepted; "KidA" is rejected at the boundary.
        for kid in ["kid-a", "kid.a", "kid:a", "kid_a", "kid_2da", "kida"] {
            let variable_name = spin_variable_name(kid, PlatformError::ConfigStore)
                .expect("should encode allowed kid characters");
            assert!(
                names.insert(variable_name.clone()),
                "Spin variable mapping must not collide for kid `{kid}` as `{variable_name}`"
            );
        }
        assert!(
            spin_variable_name("KidA", PlatformError::ConfigStore).is_err(),
            "should reject uppercase-leading kid at the encoder boundary"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn build_runtime_services_uses_noop_native_stores_without_handles() {
        let ctx = make_ctx_without_spin_context();
        let services = build_runtime_services(&ctx);

        assert!(
            services.client_info().client_ip.is_none(),
            "should not set client IP without Spin request context"
        );
        assert!(
            services
                .geo()
                .lookup(None)
                .expect("should not fail")
                .is_none(),
            "should use null geo"
        );
        assert!(
            services
                .config_store()
                .get(&StoreName::from("config"), "missing")
                .is_err(),
            "should return typed config error without injected handle"
        );
        assert!(
            services.kv_store().get_bytes("missing").await.is_err(),
            "should return typed KV error without injected handle"
        );
    }

    #[test]
    fn response_policy_strips_transfer_encoding() {
        let (headers, body) = apply_spin_response_policy(
            vec![
                ("transfer-encoding".to_string(), b"chunked".to_vec()),
                ("x-preserve".to_string(), b"yes".to_vec()),
            ],
            b"ok".to_vec(),
        )
        .expect("should apply response policy");

        assert_eq!(body, b"ok", "should preserve body");
        assert!(
            headers.iter().all(|(name, _)| name != "transfer-encoding"),
            "should strip transfer-encoding"
        );
        assert!(
            headers.iter().any(|(name, _)| name == "x-preserve"),
            "should preserve ordinary headers"
        );
    }

    #[test]
    fn response_policy_strips_headers_named_by_connection() {
        let (headers, _) = apply_spin_response_policy(
            vec![
                ("connection".to_string(), b"keep-alive, x-remove".to_vec()),
                ("x-remove".to_string(), b"drop".to_vec()),
                ("x-keep".to_string(), b"keep".to_vec()),
            ],
            b"ok".to_vec(),
        )
        .expect("should apply response policy");

        assert!(
            headers.iter().all(|(name, _)| name != "connection"),
            "should strip connection"
        );
        assert!(
            headers.iter().all(|(name, _)| name != "x-remove"),
            "should strip headers named by connection"
        );
        assert!(
            headers.iter().any(|(name, _)| name == "x-keep"),
            "should preserve ordinary response headers"
        );
    }

    #[test]
    fn response_policy_decodes_gzip_and_strips_encoding_headers() {
        let (headers, body) = apply_spin_response_policy(
            vec![
                ("content-encoding".to_string(), b"gzip".to_vec()),
                ("content-length".to_string(), b"999".to_vec()),
                ("content-type".to_string(), b"text/plain".to_vec()),
            ],
            gzip_bytes(b"hello gzip"),
        )
        .expect("should decode gzip body");

        assert_eq!(body, b"hello gzip", "should decode gzip payload");
        assert!(
            headers.iter().all(|(name, _)| {
                !name.eq_ignore_ascii_case("content-encoding")
                    && !name.eq_ignore_ascii_case("content-length")
            }),
            "should strip content-encoding and content-length after decode"
        );
        assert!(
            headers
                .iter()
                .any(|(name, value)| name == "content-type" && value == b"text/plain"),
            "should preserve ordinary headers"
        );
    }

    #[test]
    fn response_policy_decodes_brotli_and_strips_encoding_headers() {
        let (headers, body) = apply_spin_response_policy(
            vec![
                ("content-encoding".to_string(), b"br".to_vec()),
                ("content-length".to_string(), b"999".to_vec()),
            ],
            brotli_bytes(b"hello brotli"),
        )
        .expect("should decode brotli body");

        assert_eq!(body, b"hello brotli", "should decode brotli payload");
        assert!(
            headers.iter().all(|(name, _)| {
                !name.eq_ignore_ascii_case("content-encoding")
                    && !name.eq_ignore_ascii_case("content-length")
            }),
            "should strip content-encoding and content-length after decode"
        );
    }

    #[test]
    fn response_policy_preserves_unsupported_encoding() {
        let (headers, body) = apply_spin_response_policy(
            vec![("content-encoding".to_string(), b"zstd".to_vec())],
            b"raw body".to_vec(),
        )
        .expect("should preserve unsupported encoding");

        assert_eq!(body, b"raw body", "should preserve raw body bytes");
        assert!(
            headers.iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("content-encoding") && value == b"zstd"
            }),
            "should preserve unsupported encoding header"
        );
    }

    #[test]
    fn response_policy_reports_gzip_decode_failure() {
        let result = apply_spin_response_policy(
            vec![("content-encoding".to_string(), b"gzip".to_vec())],
            b"not gzip".to_vec(),
        );

        assert!(result.is_err(), "should reject invalid gzip payload");
    }

    #[test]
    fn decompression_limit_is_enforced() {
        let compressed = gzip_bytes(b"expanded body");
        let result = decompress_body(&compressed, "gzip", 4);

        assert!(
            result.is_err(),
            "should reject decoded bodies larger than the configured limit"
        );
        assert_eq!(
            MAX_DECOMPRESSED_SIZE,
            8 * 1024 * 1024,
            "production limit must stay within Spin WASM heap budget"
        );
    }

    #[test]
    fn spin_variable_name_rejects_digit_leading_key() {
        // Encoder enforces the lowercase-letter start contract at the boundary so
        // no caller can produce aliasing (e.g. "1foo" and "n1foo" would collide).
        let result = spin_variable_name("1foo", PlatformError::ConfigStore);
        assert!(
            result.is_err(),
            "should reject digit-leading key at the encoder boundary"
        );
    }

    #[test]
    fn spin_variable_name_rejects_uppercase_leading_key() {
        let result = spin_variable_name("Foo", PlatformError::ConfigStore);
        assert!(
            result.is_err(),
            "should reject uppercase-leading key at the encoder boundary"
        );
    }

    #[test]
    fn wasi_forbidden_headers_are_identified() {
        for header in &[
            "host",
            "Host",
            "HOST",
            "connection",
            "keep-alive",
            "transfer-encoding",
            "upgrade",
            "proxy-connection",
        ] {
            assert!(
                is_wasi_forbidden_outbound_header(header),
                "{header} should be forbidden in WASI HTTP outbound requests"
            );
        }
        for header in &[
            "user-agent",
            "accept",
            "content-type",
            "x-custom",
            "authorization",
        ] {
            assert!(
                !is_wasi_forbidden_outbound_header(header),
                "{header} should be allowed in WASI HTTP outbound requests"
            );
        }
    }
}
