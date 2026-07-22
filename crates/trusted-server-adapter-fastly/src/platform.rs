//! Fastly-backed implementations of the platform traits defined in
//! `trusted-server-core::platform`.

use std::io::Read as _;
use std::net::IpAddr;
use std::sync::Arc;

use bytes::Bytes;
use edgezero_adapter_fastly::key_value_store::FastlyKvStore;
use edgezero_core::key_value_store::KvError;
use error_stack::{Report, ResultExt};
use fastly::geo::{Geo, geo_lookup};
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
        .discriminator(spec.discriminator.as_deref())
}

/// Transport-timeout quantum for auction backends (see
/// [`FastlyPlatformBackend::canonicalize_transport_timeout_ms`]).
const TRANSPORT_TIMEOUT_QUANTUM_MS: u32 = 250;

/// Upper bound of the fine-grained quantum range.
///
/// Budget-bound values below this ceiling are floored to a
/// [`TRANSPORT_TIMEOUT_QUANTUM_MS`] multiple (the issue #847 behavior for the
/// default 2000 ms auction). At or above it, values snap to the coarse
/// [`TRANSPORT_TIMEOUT_COARSE_LADDER_MS`] instead so the total number of
/// distinct budget-derived buckets stays globally bounded regardless of how
/// large the configured ceiling is.
const TRANSPORT_TIMEOUT_QUANTUM_CEILING_MS: u32 = 2000;

/// Coarse rungs for budget-bound transport timeouts below one quantum,
/// ordered high to low.
///
/// Below one quantum, passing the exact wall-clock remainder through would mint
/// a distinct backend name for every millisecond in `1..250`, so the
/// near-exhausted tail alone could exceed Fastly's per-service dynamic backend
/// limit. Snapping to this finite ladder instead bounds the number of
/// budget-derived names an origin can produce. Budgets below the smallest rung
/// round to zero, which callers treat as "budget exhausted — skip the launch".
const SUB_QUANTUM_LADDER_MS: [u32; 4] = [200, 150, 100, 50];

/// Coarse rungs for budget-bound transport timeouts at or above the quantum
/// ceiling, ascending. Every rung is a [`TRANSPORT_TIMEOUT_QUANTUM_MS`]
/// multiple.
///
/// Above [`TRANSPORT_TIMEOUT_QUANTUM_CEILING_MS`], flooring to a 250 ms multiple
/// would let a large configured ceiling (e.g. 60,000 ms) mint hundreds of
/// distinct backend names — recreating the per-service dynamic backend
/// exhaustion this quantization exists to prevent. This fixed, globally finite
/// ladder caps the number of high-budget buckets instead: values are floored to
/// the greatest rung no larger than the remaining budget, and anything above
/// the top rung clamps to it. Rounding down never extends a transport cap past
/// the remaining budget.
const TRANSPORT_TIMEOUT_COARSE_LADDER_MS: [u32; 8] =
    [2000, 3000, 5000, 10000, 20000, 30000, 45000, 60000];

/// Round a budget-bound transport timeout down to a stable, globally bounded
/// bucket.
///
/// - At or above [`TRANSPORT_TIMEOUT_QUANTUM_CEILING_MS`], floors to the
///   greatest [`TRANSPORT_TIMEOUT_COARSE_LADDER_MS`] rung no larger than
///   `remaining_ms` (clamping to the top rung above it).
/// - Within the quantum range, floors to a [`TRANSPORT_TIMEOUT_QUANTUM_MS`]
///   multiple.
/// - Below one quantum, snaps down to the greatest [`SUB_QUANTUM_LADDER_MS`]
///   rung no larger than `remaining_ms` (or zero).
fn quantize_transport_timeout_ms(remaining_ms: u32) -> u32 {
    if remaining_ms >= TRANSPORT_TIMEOUT_QUANTUM_CEILING_MS {
        return TRANSPORT_TIMEOUT_COARSE_LADDER_MS
            .into_iter()
            .rev()
            .find(|&rung| rung <= remaining_ms)
            .unwrap_or(TRANSPORT_TIMEOUT_QUANTUM_CEILING_MS);
    }
    let floored = (remaining_ms / TRANSPORT_TIMEOUT_QUANTUM_MS) * TRANSPORT_TIMEOUT_QUANTUM_MS;
    if floored > 0 {
        return floored;
    }
    SUB_QUANTUM_LADDER_MS
        .into_iter()
        .find(|&rung| rung <= remaining_ms)
        .unwrap_or(0)
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

    /// Quantize the transport timeout so budget-derived values do not mint a
    /// new dynamic backend name on every request.
    ///
    /// Fastly embeds the first-byte and between-bytes timeouts in the dynamic
    /// backend name (see [`BackendConfig`]) and pools connections per backend
    /// name. A per-request wall-clock budget would otherwise defeat that
    /// pooling and accumulate registrations toward the per-service dynamic
    /// backend limit.
    ///
    /// A provider's own configured timeout is a constant, so when it is the
    /// binding constraint it is returned verbatim — including sub-quantum
    /// configured values, which must not be rounded away or the provider could
    /// never launch. Only the budget-bound value is snapped to a stable bucket
    /// via [`quantize_transport_timeout_ms`]. Rounding down never extends a
    /// transport cap past the remaining budget.
    fn canonicalize_transport_timeout_ms(&self, remaining_ms: u32, configured_ms: u32) -> u32 {
        if remaining_ms >= configured_ms {
            return configured_ms;
        }
        quantize_transport_timeout_ms(remaining_ms)
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
    response_body_expected: bool,
) -> Result<PlatformResponse, Report<PlatformError>> {
    // Pre-flight: reject oversized responses before copying bytes into WASM heap.
    // Content-Length is advisory but covers most origin responses; chunked
    // responses without it fall through to the post-materialization check below.
    // HEAD responses report the corresponding GET size but contain no body.
    if response_body_expected
        && !stream_response
        && let Some(claimed_len) = resp
            .get_header("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<usize>().ok())
        && claimed_len > MAX_PLATFORM_RESPONSE_BODY_BYTES
    {
        return Err(Report::new(PlatformError::HttpClient).attach(format!(
            "origin Content-Length {claimed_len} exceeds \
                     {MAX_PLATFORM_RESPONSE_BODY_BYTES}-byte response body limit"
        )));
    }

    let status = resp.get_status();
    let mut builder = edgezero_core::http::response_builder().status(status);
    for (name, value) in resp.get_headers() {
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let body = if !response_body_expected {
        edgezero_core::body::Body::empty()
    } else if stream_response {
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

fn apply_fastly_cache_bypass(request: &mut fastly::Request, bypass_cache: bool) {
    if bypass_cache {
        request.set_pass(true);
    }
}

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
        let response_body_expected = request.request.method() != edgezero_core::http::Method::HEAD;
        let bypass_cache = request.bypass_cache;
        let mut fastly_req = edge_request_to_fastly(request.request)?;
        if let Some(options) = image_optimizer {
            apply_fastly_image_optimizer(&mut fastly_req, options)?;
        }
        apply_fastly_cache_bypass(&mut fastly_req, bypass_cache);
        let fastly_resp = fastly_req
            .send(&backend_name)
            .change_context(PlatformError::HttpClient)?;
        fastly_response_to_platform(
            fastly_resp,
            backend_name,
            stream_response,
            response_body_expected,
        )
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
        let bypass_cache = request.bypass_cache;
        let mut fastly_req = edge_request_to_fastly(request.request)?;
        apply_fastly_cache_bypass(&mut fastly_req, bypass_cache);
        let pending = fastly_req
            .send_async(&backend_name)
            .change_context(PlatformError::HttpClient)?;
        Ok(PlatformPendingRequest::new(pending).with_backend_name(backend_name))
    }

    async fn select(
        &self,
        pending_requests: Vec<PlatformPendingRequest>,
    ) -> Result<PlatformSelectResult, Report<PlatformError>> {
        use fastly::http::request::{PendingRequest, select};

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
                    fastly_response_to_platform(fastly_resp, backend_name, false, true),
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
            discriminator: None,
        };

        let name = backend
            .predict_name(&spec)
            .expect("should compute backend name for valid spec");

        assert!(
            name.starts_with("backend_https_origin_example_com_443_fb15000_bb15000_"),
            "should match BackendConfig naming convention, got {name}"
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
            discriminator: None,
        };

        let name = backend
            .predict_name(&spec)
            .expect("should compute backend name for host header override");

        assert!(
            name.starts_with(
                "backend_https_origin_example_com_443_oh_www_example_com_fb15000_bb15000_"
            ),
            "should match BackendConfig naming convention with host header override, got {name}"
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
            discriminator: None,
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
            discriminator: None,
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
            discriminator: None,
        };

        let name = backend
            .predict_name(&spec)
            .expect("should compute name with custom timeout");

        assert!(
            name.contains("_fb2000_bb2000_"),
            "should encode 2000ms first-byte and between-bytes timeouts in name, got {name}"
        );
    }

    #[test]
    fn predict_name_matches_ensured_backend_name() {
        // The auction orchestrator maps responses back to providers by the
        // predicted backend name, so predict_name and ensure must return the
        // identical string for the same spec — a divergence would make
        // responses land in the "unknown backend" branch and drop bids
        // silently.
        let backend = FastlyPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "consistency.example.com".to_string(),
            port: None,
            host_header_override: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_millis(750),
            between_bytes_timeout: Duration::from_millis(750),
            discriminator: None,
        };

        let predicted = backend
            .predict_name(&spec)
            .expect("should predict backend name");
        let ensured = backend
            .ensure(&spec)
            .expect("should register backend for valid spec");

        assert_eq!(
            predicted, ensured,
            "predicted backend name should match the registered backend name"
        );
    }

    // --- FastlyPlatformHttpClient -------------------------------------------

    #[test]
    fn fastly_response_to_platform_allows_oversized_head_content_length() {
        let mut fastly_response = fastly::Response::from_status(200);
        fastly_response.set_header(
            fastly::http::header::CONTENT_LENGTH,
            (MAX_PLATFORM_RESPONSE_BODY_BYTES + 1).to_string(),
        );

        let platform_response =
            fastly_response_to_platform(fastly_response, "origin", false, false)
                .expect("should allow HEAD metadata for an oversized object");

        assert_eq!(
            platform_response
                .response
                .headers()
                .get(edgezero_core::http::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some("10485761"),
            "should preserve the origin Content-Length"
        );
        assert!(
            platform_response
                .response
                .into_body()
                .into_bytes()
                .unwrap_or_default()
                .is_empty(),
            "should return an empty HEAD response body"
        );
    }

    #[test]
    fn apply_fastly_cache_bypass_sets_pass_when_enabled() {
        let mut request = fastly::Request::get("https://example.com/");
        apply_fastly_cache_bypass(&mut request, true);
        assert!(
            format!("{request:?}").contains("cache_override: Pass"),
            "enabled bypass should select Fastly pass mode"
        );
    }

    #[test]
    fn fastly_response_to_platform_rejects_oversized_buffered_get_content_length() {
        let mut fastly_response = fastly::Response::from_status(200);
        fastly_response.set_header(
            fastly::http::header::CONTENT_LENGTH,
            (MAX_PLATFORM_RESPONSE_BODY_BYTES + 1).to_string(),
        );

        let error = fastly_response_to_platform(fastly_response, "origin", false, true)
            .expect_err("should reject oversized buffered GET metadata");

        assert!(
            format!("{error:?}").contains("exceeds 10485760-byte response body limit"),
            "should retain the buffered response size limit: {error:?}"
        );
    }

    #[test]
    fn apply_fastly_cache_bypass_preserves_default_when_disabled() {
        let mut request = fastly::Request::get("https://example.com/");
        apply_fastly_cache_bypass(&mut request, false);
        assert!(
            format!("{request:?}").contains("cache_override: None"),
            "disabled bypass should preserve Fastly read-through caching"
        );
    }

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

    // --- FastlyPlatformBackend::canonicalize_transport_timeout_ms -----------

    #[test]
    fn canonicalize_prefers_configured_timeout_when_budget_allows() {
        let backend = FastlyPlatformBackend;
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(2000, 1000),
            1000,
            "should use the configured timeout verbatim when the budget allows"
        );
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(2000, 100),
            100,
            "should preserve a sub-quantum configured constant — it is name-stable on its own"
        );
    }

    #[test]
    fn canonicalize_floors_budget_bound_value_to_quantum() {
        let backend = FastlyPlatformBackend;
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(999, 2000),
            750,
            "should floor a 999ms budget to the 750ms quantum bucket"
        );
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(300, 2000),
            250,
            "should floor a tight budget down to one quantum"
        );
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(250, 2000),
            250,
            "should keep an exact quantum multiple"
        );
    }

    #[test]
    fn canonicalize_snaps_sub_quantum_budget_to_bounded_ladder() {
        let backend = FastlyPlatformBackend;
        // Exact wall-clock values in 1..250 must NOT pass through — that is the
        // unbounded-cardinality regression this ladder closes.
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(249, 2000),
            200,
            "should snap a sub-quantum budget down to the greatest ladder rung, not pass 249 through"
        );
        assert_eq!(backend.canonicalize_transport_timeout_ms(200, 2000), 200);
        assert_eq!(backend.canonicalize_transport_timeout_ms(150, 2000), 150);
        assert_eq!(backend.canonicalize_transport_timeout_ms(100, 2000), 100);
        assert_eq!(backend.canonicalize_transport_timeout_ms(50, 2000), 50);
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(49, 2000),
            0,
            "a budget below the smallest rung rounds to zero (launch skipped)"
        );
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(0, 1000),
            0,
            "an exhausted budget canonicalizes to zero"
        );
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(100, 0),
            0,
            "a zero configured timeout canonicalizes to zero"
        );
    }

    #[test]
    fn canonicalize_budget_derived_names_stay_within_a_safe_cardinality() {
        // Enumerate every reachable remaining budget for a normal 2000ms
        // ceiling and confirm the number of distinct backend-name-bearing
        // transport values an origin can mint stays far below Fastly's
        // per-service dynamic backend limit (documented default 200).
        let backend = FastlyPlatformBackend;
        let configured = 2000;
        let mut distinct = std::collections::BTreeSet::new();
        for remaining in 0..=configured {
            let value = backend.canonicalize_transport_timeout_ms(remaining, configured);
            if value > 0 {
                distinct.insert(value);
            }
            // No arbitrary clock-derived value may leak: every canonical value
            // is either a quantum multiple or one of the bounded ladder rungs.
            assert!(
                value == 0
                    || value % TRANSPORT_TIMEOUT_QUANTUM_MS == 0
                    || SUB_QUANTUM_LADDER_MS.contains(&value),
                "canonical value {value}ms (from remaining {remaining}ms) is neither a quantum \
                 multiple nor a ladder rung"
            );
        }
        assert!(
            distinct.len() <= 16,
            "budget-derived transport values should stay well under the dynamic backend limit, \
             got {} distinct values: {distinct:?}",
            distinct.len()
        );
    }

    #[test]
    fn canonicalize_budget_derived_names_stay_bounded_for_large_ceiling() {
        // A large configured ceiling (e.g. a 60s mediator budget) must not let
        // the budget-derived buckets grow with the ceiling. Without the coarse
        // ladder a 60,000ms ceiling would mint ~240 distinct 250ms buckets and
        // blow past Fastly's documented per-service dynamic backend limit (200).
        let backend = FastlyPlatformBackend;
        let configured = 60_000;
        let mut distinct = std::collections::BTreeSet::new();
        for remaining in 0..=configured {
            let value = backend.canonicalize_transport_timeout_ms(remaining, configured);
            if value > 0 {
                distinct.insert(value);
            }
            assert!(
                value == 0
                    || value % TRANSPORT_TIMEOUT_QUANTUM_MS == 0
                    || SUB_QUANTUM_LADDER_MS.contains(&value),
                "canonical value {value}ms (from remaining {remaining}ms) is neither a quantum \
                 multiple nor a ladder rung"
            );
        }
        // A budget above the top coarse rung must clamp to it, not open a new
        // bucket per 250ms step.
        assert_eq!(
            backend.canonicalize_transport_timeout_ms(120_000, 240_000),
            60_000,
            "a budget above the top coarse rung clamps to it"
        );
        assert!(
            distinct.len() <= 20,
            "large-ceiling budget-derived values must stay bounded, got {} distinct values: {distinct:?}",
            distinct.len()
        );
    }

    // --- FastlyPlatformBackend::predict_name discriminator ------------------

    #[test]
    fn predict_name_includes_provider_discriminator() {
        let backend = FastlyPlatformBackend;
        let base = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "gateway.example.com".to_string(),
            port: None,
            host_header_override: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_millis(750),
            between_bytes_timeout: Duration::from_millis(750),
            discriminator: Some("prebid".to_string()),
        };
        let prebid_name = backend
            .predict_name(&base)
            .expect("should predict name with discriminator");
        assert!(
            prebid_name.contains("_p_prebid"),
            "should fold the provider discriminator into the name, got {prebid_name}"
        );

        // Same origin + same transport timeout, different provider → distinct
        // backend names, so auction response correlation cannot cross them.
        let aps = PlatformBackendSpec {
            discriminator: Some("aps".to_string()),
            ..base.clone()
        };
        let aps_name = backend
            .predict_name(&aps)
            .expect("should predict name for the second provider");
        assert_ne!(
            prebid_name, aps_name,
            "two providers on one origin must not share a backend name"
        );
    }
}
