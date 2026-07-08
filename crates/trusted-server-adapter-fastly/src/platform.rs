//! Fastly-backed implementations of the platform traits defined in
//! `trusted-server-core::platform`.

use std::io::Read as _;
use std::net::IpAddr;
use std::sync::Arc;

use bytes::Bytes;
use edgezero_adapter_fastly::key_value_store::FastlyKvStore;
use edgezero_core::key_value_store::KvError;
use error_stack::{Report, ResultExt};
use fastly::geo::{geo_lookup, Geo};
use fastly::{ConfigStore, Request, SecretStore};

use crate::backend::BackendConfig;
pub(crate) use trusted_server_core::platform::UnavailableKvStore;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore, PlatformError,
    PlatformGeo, PlatformHttpClient, PlatformHttpRequest, PlatformImageOptimizerCrop,
    PlatformImageOptimizerCropMode, PlatformImageOptimizerOptions, PlatformImageOptimizerParams,
    PlatformImageOptimizerRegion, PlatformKvStore, PlatformPendingRequest, PlatformResponse,
    PlatformSecretStore, PlatformSelectResult, StoreId, StoreName,
};

// ---------------------------------------------------------------------------
// FastlyPlatformConfigStore
// ---------------------------------------------------------------------------

/// Fastly [`ConfigStore`]-backed implementation of [`PlatformConfigStore`].
///
/// Stateless — the store name is supplied per call, matching the trait
/// signature. This replaces the store-name-at-construction pattern of
/// the legacy `FastlyConfigStore` (removed).
///
/// # Write cost
///
/// `put` and `delete` each perform a synchronous outbound HTTPS request to the
/// Fastly management API (`api.fastly.com`). Callers that issue many writes in
/// one request pay one round-trip per call. The `"api-keys"` secret store is
/// opened per call to read the management token; the Fastly Compute SDK caches
/// the open handle so that cost is negligible.
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
/// store-name-at-construction pattern of the legacy `FastlySecretStore`
/// (removed).
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
        .host_header_override(spec.host_header_override.as_deref())
        .certificate_check(spec.certificate_check)
        .first_byte_timeout(spec.first_byte_timeout)
        .between_bytes_timeout(spec.between_bytes_timeout)
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

fn fastly_image_optimizer_region(
    region: &str,
) -> Result<fastly::image_optimizer::ImageOptimizerRegion, Report<PlatformError>> {
    use fastly::image_optimizer::ImageOptimizerRegion;

    match PlatformImageOptimizerRegion::parse(region) {
        Some(PlatformImageOptimizerRegion::UsEast) => Ok(ImageOptimizerRegion::UsEast),
        Some(PlatformImageOptimizerRegion::UsCentral) => Ok(ImageOptimizerRegion::UsCentral),
        Some(PlatformImageOptimizerRegion::UsWest) => Ok(ImageOptimizerRegion::UsWest),
        Some(PlatformImageOptimizerRegion::EuCentral) => Ok(ImageOptimizerRegion::EuCentral),
        Some(PlatformImageOptimizerRegion::EuWest) => Ok(ImageOptimizerRegion::EuWest),
        Some(PlatformImageOptimizerRegion::Asia) => Ok(ImageOptimizerRegion::Asia),
        Some(PlatformImageOptimizerRegion::Australia) => Ok(ImageOptimizerRegion::Australia),
        None => Err(Report::new(PlatformError::HttpClient)
            .attach(format!("unsupported Image Optimizer region: {region}"))),
    }
}

fn fastly_image_optimizer_format(
    format: &str,
) -> Result<fastly::image_optimizer::Format, Report<PlatformError>> {
    use fastly::image_optimizer::Format;

    match format.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(Format::Auto),
        "avif" => Ok(Format::AVIF),
        "gif" => Ok(Format::GIF),
        "jpeg" | "jpg" => Ok(Format::JPEG),
        "jxl" | "jpegxl" => Ok(Format::JPEGXL),
        "mp4" => Ok(Format::MP4),
        "png" => Ok(Format::PNG),
        "webp" => Ok(Format::WebP),
        other => Err(Report::new(PlatformError::HttpClient)
            .attach(format!("unsupported Image Optimizer format: {other}"))),
    }
}

fn fastly_resize_filter(
    resize_filter: &str,
) -> Result<fastly::image_optimizer::ResizeAlgorithm, Report<PlatformError>> {
    use fastly::image_optimizer::ResizeAlgorithm;

    match resize_filter.trim().to_ascii_lowercase().as_str() {
        "nearest" => Ok(ResizeAlgorithm::Nearest),
        "bilinear" => Ok(ResizeAlgorithm::Bilinear),
        "bicubic" => Ok(ResizeAlgorithm::Bicubic),
        "lanczos2" => Ok(ResizeAlgorithm::Lanczos2),
        "lanczos3" => Ok(ResizeAlgorithm::Lanczos3),
        other => Err(Report::new(PlatformError::HttpClient).attach(format!(
            "unsupported Image Optimizer resize filter: {other}"
        ))),
    }
}

fn fastly_crop(crop: &PlatformImageOptimizerCrop) -> fastly::image_optimizer::Crop {
    use fastly::image_optimizer::{Area, Crop, CropMode, PointOrOffset, Position};

    let position = match (crop.offset_x, crop.offset_y) {
        (Some(x), Some(y)) => Some(Position {
            x: Some(PointOrOffset::Offset(x)),
            y: Some(PointOrOffset::Offset(y)),
        }),
        _ => None,
    };
    let mode = crop
        .mode
        .map(|PlatformImageOptimizerCropMode::Smart| CropMode::Smart);

    Crop {
        size: Area::AspectRatio((crop.width, crop.height)),
        position,
        mode,
    }
}

fn apply_fastly_image_optimizer_params(
    target: &mut fastly::image_optimizer::ImageOptimizerOptions,
    params: PlatformImageOptimizerParams,
) -> Result<(), Report<PlatformError>> {
    use fastly::image_optimizer::PixelsOrPercentage;

    if let Some(format) = params.format {
        target.format = Some(fastly_image_optimizer_format(&format)?);
    }
    if let Some(quality) = params.quality {
        target.quality = Some(quality);
    }
    if let Some(resize_filter) = params.resize_filter {
        target.resize_filter = Some(fastly_resize_filter(&resize_filter)?);
    }
    if let Some(width) = params.width {
        target.width = Some(PixelsOrPercentage::Pixels(width));
    }
    if let Some(height) = params.height {
        target.height = Some(PixelsOrPercentage::Pixels(height));
    }
    if let Some(crop) = params.crop {
        target.crop = Some(fastly_crop(&crop));
    }

    Ok(())
}

fn apply_fastly_image_optimizer(
    req: &mut fastly::Request,
    options: PlatformImageOptimizerOptions,
) -> Result<(), Report<PlatformError>> {
    let region = fastly_image_optimizer_region(&options.region)?;
    let mut fastly_options = fastly::image_optimizer::ImageOptimizerOptions::from_region(region);
    fastly_options.preserve_query_string_on_origin_request =
        Some(options.preserve_query_string_on_origin_request);
    apply_fastly_image_optimizer_params(&mut fastly_options, options.params)?;
    req.set_image_optimizer(fastly_options);
    Ok(())
}

/// Convert a platform-neutral [`edgezero_core::http::Request`] to a [`fastly::Request`].
///
/// Only buffered `Body::Once` bodies are supported on this path.
///
/// # Errors
///
/// Returns [`PlatformError::HttpClient`] when the request body is streaming.
fn edge_request_to_fastly(
    request: edgezero_core::http::Request,
) -> Result<fastly::Request, Report<PlatformError>> {
    let (parts, body) = request.into_parts();
    let mut fastly_req = fastly::Request::new(parts.method, parts.uri.to_string());
    for (name, value) in parts.headers.iter() {
        // `fastly::Request::new` derives a Host header from the request URI, so
        // appending the edge request's own Host would leave a duplicate. Replace
        // it instead to keep the in-memory request well-formed. The Host actually
        // sent on the wire is still governed by the backend's `override_host`
        // (see `BackendConfig::ensure`), which forces the value regardless.
        if name == edgezero_core::http::header::HOST {
            fastly_req.set_header(name.as_str(), value.as_bytes());
        } else {
            fastly_req.append_header(name.as_str(), value.as_bytes());
        }
    }
    match body {
        edgezero_core::body::Body::Once(bytes) => {
            if !bytes.is_empty() {
                fastly_req.set_body(bytes.to_vec());
            }
        }
        edgezero_core::body::Body::Stream(_) => {
            return Err(Report::new(PlatformError::HttpClient)
                .attach("streaming request body is not supported by Fastly request conversion"));
        }
    }
    Ok(fastly_req)
}

/// Maximum origin response body size copied into WASM heap.
///
/// `take_body_bytes()` copies the full origin response into a single
/// allocation.  This cap prevents oversized origin responses from exhausting
/// the WASM address space.  The Content-Length pre-check avoids the copy
/// entirely for responses that declare their size.
const MAX_PLATFORM_RESPONSE_BODY_BYTES: usize = 10 * 1024 * 1024; // 10 MiB

fn fastly_body_to_edge_stream(body: fastly::Body) -> edgezero_core::body::Body {
    let stream = futures::stream::unfold(Some(body), |state| async move {
        let mut body = state?;
        let mut chunk = vec![0; 8192];
        match body.read(&mut chunk) {
            Ok(0) => None,
            Ok(bytes_read) => {
                chunk.truncate(bytes_read);
                Some((Ok(Bytes::from(chunk)), Some(body)))
            }
            Err(err) => Some((Err(err), None)),
        }
    });

    edgezero_core::body::Body::from_stream(stream)
}

/// Convert a [`fastly::Response`] to a [`PlatformResponse`] with the given backend name.
fn fastly_response_to_platform(
    mut resp: fastly::Response,
    backend_name: impl Into<String>,
    stream_response: bool,
) -> Result<PlatformResponse, Report<PlatformError>> {
    // Pre-flight: reject oversized responses before copying bytes into WASM heap.
    // Content-Length is advisory but covers most origin responses; chunked
    // responses without it fall through to the post-materialization check below.
    if !stream_response {
        if let Some(claimed_len) = resp
            .get_header("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<usize>().ok())
        {
            if claimed_len > MAX_PLATFORM_RESPONSE_BODY_BYTES {
                return Err(Report::new(PlatformError::HttpClient).attach(format!(
                    "origin Content-Length {claimed_len} exceeds \
                     {MAX_PLATFORM_RESPONSE_BODY_BYTES}-byte response body limit"
                )));
            }
        }
    }

    let status = resp.get_status();
    let mut builder = edgezero_core::http::response_builder().status(status);
    for (name, value) in resp.get_headers() {
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let body = if stream_response {
        fastly_body_to_edge_stream(resp.take_body())
    } else {
        let body_bytes = resp.take_body_bytes();

        // Belt-and-suspenders: catches chunked responses without Content-Length.
        if body_bytes.len() > MAX_PLATFORM_RESPONSE_BODY_BYTES {
            return Err(Report::new(PlatformError::HttpClient).attach(format!(
                "origin response body {} bytes exceeds \
                 {MAX_PLATFORM_RESPONSE_BODY_BYTES}-byte limit",
                body_bytes.len()
            )));
        }

        edgezero_core::body::Body::from(body_bytes)
    };
    let edge_response = builder
        .body(body)
        .change_context(PlatformError::HttpClient)?;
    Ok(PlatformResponse::new(edge_response).with_backend_name(backend_name))
}

// ---------------------------------------------------------------------------
// FastlyPlatformHttpClient
// ---------------------------------------------------------------------------

/// Fastly implementation of [`PlatformHttpClient`].
///
/// - [`send`](PlatformHttpClient::send) converts the platform request to a
///   `fastly::Request`, applies Image Optimizer metadata when present, calls
///   `.send()`, and wraps the response. Asset requests can ask to preserve the
///   response body as a stream instead of buffering it into a single `Vec`.
/// - [`send_async`](PlatformHttpClient::send_async) converts the request and
///   calls `.send_async()`. It rejects Image Optimizer metadata because Fastly's
///   async request path does not expose the IO attachment used by asset routes.
/// - [`select`](PlatformHttpClient::select) downcasts each
///   [`PlatformPendingRequest`] back to `fastly::PendingRequest` and calls
///   `fastly::http::request::select()`.
pub struct FastlyPlatformHttpClient;

#[async_trait::async_trait(?Send)]
impl PlatformHttpClient for FastlyPlatformHttpClient {
    fn supports_streaming_responses(&self) -> bool {
        true
    }

    async fn send(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        let backend_name = request.backend_name.clone();
        let image_optimizer = request.image_optimizer;
        let stream_response = request.stream_response;
        let mut fastly_req = edge_request_to_fastly(request.request)?;
        if let Some(options) = image_optimizer {
            apply_fastly_image_optimizer(&mut fastly_req, options)?;
        }
        let fastly_resp = fastly_req
            .send(&backend_name)
            .change_context(PlatformError::HttpClient)?;
        fastly_response_to_platform(fastly_resp, backend_name, stream_response)
    }

    async fn send_async(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
        let backend_name = request.backend_name.clone();
        if request.image_optimizer.is_some() {
            return Err(Report::new(PlatformError::HttpClient)
                .attach("Image Optimizer is not supported with Fastly send_async"));
        }
        if request.stream_response {
            return Err(Report::new(PlatformError::HttpClient)
                .attach("streaming responses are not supported with Fastly send_async"));
        }
        let fastly_req = edge_request_to_fastly(request.request)?;
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

        for platform_req in pending_requests {
            let inner = platform_req.downcast::<PendingRequest>().map_err(|platform_req| {
                let backend_name = platform_req.backend_name().unwrap_or("<unknown>");
                Report::new(PlatformError::HttpClient).attach(format!(
                    "PlatformPendingRequest inner type is not fastly::PendingRequest for backend '{backend_name}'"
                ))
            })?;
            fastly_pending.push(inner);
        }

        let (result, remaining_fastly) = select(fastly_pending);

        // Fastly's select() does not preserve input order for remaining requests,
        // so positional backend-name re-association is unreliable. Backend names
        // are re-derived from get_backend_name() when each remaining request completes.
        let remaining: Vec<PlatformPendingRequest> = remaining_fastly
            .into_iter()
            .map(PlatformPendingRequest::new)
            .collect();

        let (ready, failed_backend_name) = match result {
            Ok(fastly_resp) => {
                let Some(backend_name) = fastly_resp.get_backend_name().map(str::to_string) else {
                    return Err(Report::new(PlatformError::HttpClient)
                        .attach("select: response has no backend name; correlation impossible"));
                };
                (
                    fastly_response_to_platform(fastly_resp, backend_name, false),
                    None,
                )
            }
            Err(e) => {
                let failed_name = e.backend_name().to_string();
                (
                    Err(Report::new(PlatformError::HttpClient).attach(format!(
                        "fastly select error for backend '{failed_name}': {e}"
                    ))),
                    Some(failed_name),
                )
            }
        };

        Ok(PlatformSelectResult {
            ready,
            remaining,
            failed_backend_name,
        })
    }
}

// ---------------------------------------------------------------------------
// FastlyPlatformGeo
// ---------------------------------------------------------------------------

/// Convert a Fastly [`Geo`] value into a platform-neutral [`GeoInfo`].
///
/// Shared by `FastlyPlatformGeo::lookup` in `trusted-server-adapter-fastly` so
/// that field mapping is never duplicated.
fn geo_from_fastly(geo: &Geo) -> GeoInfo {
    GeoInfo {
        city: geo.city().to_string(),
        country: geo.country_code().to_string(),
        continent: format!("{:?}", geo.continent()),
        latitude: geo.latitude(),
        longitude: geo.longitude(),
        metro_code: geo.metro_code(),
        region: geo.region().map(str::to_string),
        asn: None,
    }
}

/// Fastly geo-lookup implementation of [`PlatformGeo`].
pub struct FastlyPlatformGeo;

impl PlatformGeo for FastlyPlatformGeo {
    fn lookup(&self, client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(client_ip
            .and_then(geo_lookup)
            .map(|geo| geo_from_fastly(&geo)))
    }
}

/// Extract [`ClientInfo`] from the original Fastly request.
///
/// Fastly's TLS, JA4, and HTTP/2 fingerprint accessors only return real values
/// on the client request before it is converted to platform HTTP types. This
/// must therefore be called by the `EdgeZero` entry point while the original
/// [`fastly::Request`] is still available. The result is stored in request
/// extensions so `build_per_request_services` can read back metadata the
/// reconstructed request cannot expose.
#[must_use]
pub fn client_info_from_request(req: &Request) -> ClientInfo {
    ClientInfo {
        client_ip: req.get_client_ip_addr(),
        tls_protocol: req.get_tls_protocol().ok().flatten().map(str::to_string),
        tls_cipher: req
            .get_tls_cipher_openssl_name()
            .ok()
            .flatten()
            .map(str::to_string),
        tls_ja4: req.get_tls_ja4().map(str::to_string),
        h2_fingerprint: req.get_client_h2_fingerprint().map(str::to_string),
        server_hostname: std::env::var("FASTLY_HOSTNAME").ok(),
        server_region: std::env::var("FASTLY_REGION").ok(),
    }
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
    use std::io;
    use std::time::Duration;

    use super::*;
    use edgezero_core::body::Body;
    use edgezero_core::http::request_builder;

    #[test]
    fn edge_request_to_fastly_replaces_url_derived_host_header() {
        let request = request_builder()
            .method("GET")
            .uri("https://origin.example.com/")
            .header(edgezero_core::http::header::HOST, "www.example.com")
            .body(Body::empty())
            .expect("should build request");

        let fastly_req = edge_request_to_fastly(request).expect("should convert request");

        assert_eq!(
            fastly_req.get_header_str(fastly::http::header::HOST),
            Some("www.example.com"),
            "should replace the URL-derived Host instead of appending a duplicate"
        );
    }

    // --- FastlyPlatformBackend::predict_name --------------------------------

    #[test]
    fn predict_name_produces_same_name_as_backend_config() {
        let backend = FastlyPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "origin.example.com".to_string(),
            port: None,
            host_header_override: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_secs(15),
            between_bytes_timeout: Duration::from_secs(15),
        };

        let name = backend
            .predict_name(&spec)
            .expect("should compute backend name for valid spec");

        assert_eq!(
            name, "backend_https_origin_example_com_443_fb15000_bb15000",
            "should match BackendConfig naming convention"
        );
    }

    #[test]
    fn predict_name_includes_host_header_override_suffix() {
        let backend = FastlyPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "origin.example.com".to_string(),
            port: None,
            host_header_override: Some("www.example.com".to_string()),
            certificate_check: true,
            first_byte_timeout: Duration::from_secs(15),
            between_bytes_timeout: Duration::from_secs(15),
        };

        let name = backend
            .predict_name(&spec)
            .expect("should compute backend name for host header override");

        assert_eq!(
            name, "backend_https_origin_example_com_443_oh_www_example_com_fb15000_bb15000",
            "should match BackendConfig naming convention with host header override"
        );
    }

    #[test]
    fn predict_name_includes_nocert_suffix_when_cert_check_disabled() {
        let backend = FastlyPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "origin.example.com".to_string(),
            port: None,
            host_header_override: None,
            certificate_check: false,
            first_byte_timeout: Duration::from_secs(15),
            between_bytes_timeout: Duration::from_secs(15),
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
            host_header_override: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_secs(15),
            between_bytes_timeout: Duration::from_secs(15),
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
            host_header_override: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_millis(2000),
            between_bytes_timeout: Duration::from_millis(2000),
        };

        let name = backend
            .predict_name(&spec)
            .expect("should compute name with custom timeout");

        assert!(
            name.ends_with("_fb2000_bb2000"),
            "should encode 2000ms first-byte and between-bytes timeouts in name"
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
        let wrong = PlatformPendingRequest::new(42u32).with_backend_name("origin-a");
        let err = futures::executor::block_on(client.select(vec![wrong]))
            .expect_err("should return error for wrong inner type");

        assert!(
            matches!(err.current_context(), &PlatformError::HttpClient),
            "should be HttpClient error, got: {:?}",
            err.current_context()
        );
        assert!(
            format!("{err:?}").contains("origin-a"),
            "should include backend name in error report: {err:?}"
        );
    }

    #[test]
    fn fastly_platform_http_client_send_returns_error_for_streaming_body() {
        let client = FastlyPlatformHttpClient;
        let request = request_builder()
            .method("POST")
            .uri("https://example.com/")
            .body(Body::from_stream(futures::stream::empty::<
                Result<_, io::Error>,
            >()))
            .expect("should build streaming test request");

        let err = futures::executor::block_on(
            client.send(PlatformHttpRequest::new(request, "nonexistent-backend")),
        )
        .expect_err("should reject streaming request bodies before sending");

        assert!(
            matches!(err.current_context(), &PlatformError::HttpClient),
            "should be HttpClient error, got: {:?}",
            err.current_context()
        );
        assert!(
            format!("{err:?}").contains("streaming request body"),
            "should describe the unsupported streaming body: {err:?}"
        );
    }

    #[test]
    fn fastly_platform_http_client_send_async_rejects_image_optimizer_metadata() {
        let client = FastlyPlatformHttpClient;
        let request = request_builder()
            .method("GET")
            .uri("https://example.com/image.jpg")
            .body(Body::empty())
            .expect("should build test request");
        let platform_request = PlatformHttpRequest::new(request, "nonexistent-backend")
            .with_image_optimizer(PlatformImageOptimizerOptions::new(
                "us_east",
                PlatformImageOptimizerParams::default(),
            ));

        let err = futures::executor::block_on(client.send_async(platform_request))
            .expect_err("should reject async Image Optimizer requests");

        assert!(
            format!("{err:?}").contains("Image Optimizer"),
            "should explain unsupported async IO path: {err:?}"
        );
    }

    #[test]
    fn fastly_platform_http_client_send_async_rejects_stream_response() {
        let client = FastlyPlatformHttpClient;
        let request = request_builder()
            .method("GET")
            .uri("https://example.com/image.jpg")
            .body(Body::empty())
            .expect("should build test request");
        let platform_request =
            PlatformHttpRequest::new(request, "nonexistent-backend").with_stream_response();

        let err = futures::executor::block_on(client.send_async(platform_request))
            .expect_err("should reject async streaming-response requests");

        assert!(
            format!("{err:?}").contains("streaming responses"),
            "should explain unsupported async streaming-response path: {err:?}"
        );
    }

    #[test]
    fn fastly_platform_http_client_send_async_returns_error_for_streaming_body() {
        let client = FastlyPlatformHttpClient;
        let request = request_builder()
            .method("POST")
            .uri("https://example.com/")
            .body(Body::from_stream(futures::stream::empty::<
                Result<_, io::Error>,
            >()))
            .expect("should build streaming test request");

        let err = futures::executor::block_on(
            client.send_async(PlatformHttpRequest::new(request, "nonexistent-backend")),
        )
        .expect_err("should reject streaming request bodies before launching async send");

        assert!(
            matches!(err.current_context(), &PlatformError::HttpClient),
            "should be HttpClient error, got: {:?}",
            err.current_context()
        );
        assert!(
            format!("{err:?}").contains("streaming request body"),
            "should describe the unsupported streaming body: {err:?}"
        );
    }
}
