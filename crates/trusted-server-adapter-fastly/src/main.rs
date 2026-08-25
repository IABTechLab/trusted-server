use std::sync::Arc;
use std::time::Instant;

use edgezero_adapter_fastly::config_store::FastlyConfigStore as EdgeZeroFastlyConfigStore;
use edgezero_adapter_fastly::request::into_core_request;
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::config_store::ConfigStoreHandle;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{
    HeaderName, HeaderValue, Request as HttpRequest, Response as HttpResponse,
};
use edgezero_core::response::IntoResponse;
use error_stack::Report;
use fastly::http::Method as FastlyMethod;
use fastly::{Request as FastlyRequest, Response as FastlyResponse};

use trusted_server_core::access_telemetry::{AccessTelemetrySnapshot, RouteClass, RouteMetadata};
use trusted_server_core::cache_policy::{
    EdgeCacheHeader, cache_control_headers_are_private_or_no_store,
};
use trusted_server_core::constants::{
    ENV_FASTLY_IS_STAGING, ENV_FASTLY_POP, ENV_FASTLY_SERVICE_ID, ENV_FASTLY_SERVICE_VERSION,
};
use trusted_server_core::ec::device::DeviceSignals;
use trusted_server_core::ec::finalize::ec_finalize_response;
use trusted_server_core::ec::kv::KvIdentityGraph;
use trusted_server_core::ec::pull_sync::{
    PullSyncContext, build_pull_sync_context, dispatch_pull_sync,
};
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::geo::GeoLookupState;
use trusted_server_core::integrations::RequestFilterEffects;
use trusted_server_core::platform::PlatformGeo as _;
use trusted_server_core::platform::{RuntimeServices, TimedKvStore};
use trusted_server_core::proxy::{AssetProxyCachePolicy, stream_asset_body};
use trusted_server_core::publisher::TemplateCacheResponseState;
use trusted_server_core::request_timing::{Phase, RequestTimings};
use trusted_server_core::response_privacy::TerminalPrivateResponse;
use trusted_server_core::settings::Settings;

mod app;
mod backend;
mod compat;
mod ec_kv;
mod esi_assembly;
mod logging;
mod management_api;
mod middleware;
mod platform;
mod rate_limiter;
mod template_cache;
mod tinybird;

use crate::app::{EcFinalizeState, TrustedServerApp, load_settings_from_config_store};
use crate::ec_kv::FastlyEcKvStore;
use crate::middleware::{HEADER_X_TS_FINALIZED, apply_finalize_headers, resolve_geo_for_response};
use crate::platform::{FastlyPlatformGeo, client_info_from_request};
use crate::rate_limiter::{FastlyRateLimiter, RATE_COUNTER_NAME};

const TRUSTED_SERVER_CONFIG_STORE: &str = "trusted_server_config";

/// `Server-Timing` header name. Not present in the `http` crate's `header`
/// module (unlike `CACHE_CONTROL` etc.), so declared locally following the
/// same `HeaderName::from_static` pattern used in
/// `trusted_server_core::constants`.
const HEADER_SERVER_TIMING: HeaderName = HeaderName::from_static("server-timing");

/// Opens the Fastly Config Store used by the `EdgeZero` dispatcher.
///
/// # Errors
///
/// Returns [`fastly::Error`] if the config store cannot be opened.
fn open_trusted_server_config_store() -> Result<ConfigStoreHandle, fastly::Error> {
    let store = EdgeZeroFastlyConfigStore::try_open(TRUSTED_SERVER_CONFIG_STORE).map_err(|e| {
        fastly::Error::msg(format!(
            "failed to open config store `{TRUSTED_SERVER_CONFIG_STORE}`: {e}"
        ))
    })?;
    Ok(ConfigStoreHandle::new(Arc::new(store)))
}

fn health_response(req: &FastlyRequest) -> Option<FastlyResponse> {
    if req.get_method() == FastlyMethod::GET && req.get_path() == "/health" {
        return Some(FastlyResponse::from_status(200).with_body_text_plain("ok"));
    }

    None
}

/// Entry point for the Fastly Compute program.
///
/// Uses an undecorated `main()` with `FastlyRequest::from_client()` instead of
/// `#[fastly::main]` so the `EdgeZero` streaming publisher path can call
/// [`fastly::Response::stream_to_client`] explicitly.
fn main() {
    let req = FastlyRequest::from_client();

    // Health probe bypasses logging, settings, and app construction as a cheap liveness signal.
    if let Some(response) = health_response(&req) {
        response.send_to_client();
        return;
    }

    logging::init_logger();
    edgezero_main(req);
}

/// Handles a request through the `EdgeZero` router path.
fn edgezero_main(mut req: FastlyRequest) {
    // Short-circuit the JA4 debug probe before app construction. Must run here
    // because TLS/JA4 accessors are only available on FastlyRequest before
    // conversion to edgezero types.
    if req.get_method() == FastlyMethod::GET && req.get_path() == "/_ts/debug/ja4" {
        match load_settings_from_config_store() {
            Ok(settings) if settings.debug.ja4_endpoint_enabled => {
                build_ja4_debug_response(&req).send_to_client();
            }
            Ok(_) => {
                FastlyResponse::from_status(fastly::http::StatusCode::NOT_FOUND).send_to_client();
            }
            Err(e) => {
                log::warn!("EdgeZero JA4 endpoint: failed to load settings: {e:?}");
                FastlyResponse::from_status(fastly::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_body_text_plain("Internal Server Error")
                    .send_to_client();
            }
        }
        return;
    }

    let timings = RequestTimings::new();

    let (config_store, app, app_state) = {
        let _appbuild = timings.span(Phase::AppBuild);
        let config_store = match open_trusted_server_config_store() {
            Ok(cs) => cs,
            Err(e) => {
                log::error!("failed to open config store: {e}");
                FastlyResponse::from_status(fastly::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_body_text_plain("Internal Server Error")
                    .send_to_client();
                return;
            }
        };
        let (app, app_state) = TrustedServerApp::build_app_with_state();
        (config_store, app, app_state)
    };
    let settings_snapshot = app_state.as_ref().map(|state| Arc::clone(&state.settings));
    let server_timing_enabled = settings_snapshot
        .as_deref()
        .is_some_and(|settings| settings.observability.server_timing_enabled);
    // Both read once here rather than at each `send_edgezero_response` call
    // site: if `app_state` failed to build, there is no settings snapshot to
    // read them from at all, so every call site would need the same
    // degraded-mode fallback. `access_sample_rate` defaults to `0.0` (never
    // sampled in) and `publisher_domain` to `"unknown"` in that case.
    let access_sample_rate = settings_snapshot
        .as_deref()
        .map_or(0.0, |settings| settings.tinybird.access_sample_rate);
    let publisher_domain = settings_snapshot.as_deref().map_or_else(
        || "unknown".to_owned(),
        |settings| settings.publisher.domain.clone(),
    );

    // Strip client-spoofable forwarded headers before dispatch.
    compat::sanitize_fastly_forwarded_headers(&mut req);

    // Re-inject a trusted TLS scheme signal after sanitization has stripped any
    // client-sent fastly-ssl header. Setting it from Fastly's native TLS
    // metadata here is authoritative. detect_request_scheme in http_util checks
    // this header so scheme-sensitive logic produces https URLs on HTTPS traffic.
    if req.get_tls_protocol().ok().flatten().is_some()
        || req.get_tls_cipher_openssl_name().ok().flatten().is_some()
    {
        req.set_header("fastly-ssl", "1");
    }

    // Capture client IP and method before the request is consumed by
    // dispatch: nothing else survives to the freeze point in
    // `send_edgezero_response`, which only receives the response.
    let client_ip = req.get_client_ip_addr();
    let request_method = req.get_method_str().to_owned();

    // Strip any client-supplied x-ts-tls-* headers before injecting the trusted
    // values from the Fastly SDK. Must run after sanitize_fastly_forwarded_headers.
    req.remove_header("x-ts-tls-protocol");
    req.remove_header("x-ts-tls-cipher");
    if let Some(proto) = req.get_tls_protocol().ok().flatten().map(str::to_owned) {
        req.set_header("x-ts-tls-protocol", proto);
    }
    if let Some(cipher) = req
        .get_tls_cipher_openssl_name()
        .ok()
        .flatten()
        .map(str::to_owned)
    {
        req.set_header("x-ts-tls-cipher", cipher);
    }

    // Capture metadata from the original FastlyRequest before conversion. These
    // accessors only return real values on the client request, so store them in
    // request extensions for build_per_request_services and EC bot classification.
    let client_info = client_info_from_request(&req);
    let device_signals = derive_device_signals(&req);

    // Dispatch directly through the EdgeZero router without an intermediate
    // fastly::Response conversion. That preserves duplicate header values such
    // as multiple Set-Cookie headers.
    let mut response = match into_core_request(req) {
        Ok(mut core_req) => {
            core_req.extensions_mut().insert(config_store);
            core_req.extensions_mut().insert(device_signals);
            core_req.extensions_mut().insert(client_info);
            core_req.extensions_mut().insert(timings.clone());
            match futures::executor::block_on(app.router().oneshot(core_req)) {
                Ok(response) => response,
                Err(error) => edge_error_response(error),
            }
        }
        Err(e) => {
            log::error!("EdgeZero request conversion failed: {e}");
            FastlyResponse::from_status(fastly::http::StatusCode::INTERNAL_SERVER_ERROR)
                .with_body_text_plain("Internal Server Error")
                .send_to_client();
            return;
        }
    };

    // Pop response extensions before the Fastly conversion, which drops them.
    let ec_state = response.extensions_mut().remove::<EcFinalizeState>();
    let asset_cache_policy = response.extensions_mut().remove::<AssetProxyCachePolicy>();
    let request_filter_effects = response.extensions_mut().remove::<RequestFilterEffects>();
    // Read rather than pop: the access-telemetry snapshot built later in
    // `send_edgezero_response` reads this same extension, so it must still
    // be attached to `response` at that point.
    let geo_lookup_state = response
        .extensions()
        .get::<GeoLookupState>()
        .cloned()
        .unwrap_or(GeoLookupState::NotAttempted);

    if !take_finalize_sentinel(&mut response) {
        if let Some(settings) = settings_snapshot.as_deref() {
            apply_entry_point_finalize_headers(
                settings,
                &mut response,
                client_ip,
                &geo_lookup_state,
                &timings,
            );
        } else {
            match load_settings_from_config_store() {
                Ok(settings) => {
                    apply_entry_point_finalize_headers(
                        &settings,
                        &mut response,
                        client_ip,
                        &geo_lookup_state,
                        &timings,
                    );
                }
                Err(e) => {
                    log::warn!("entry-point finalize skipped: failed to reload settings: {e:?}");
                }
            }
        }
    }

    if let Some(policy) = asset_cache_policy {
        policy.apply_after_route_finalization(&mut response, EdgeCacheHeader::SurrogateControl);
    }

    if let Some(ec_state) = ec_state {
        if let Some(settings) = settings_snapshot.as_deref() {
            match apply_edgezero_ec_finalize(settings, &ec_state, &mut response, &timings) {
                Ok(partner_registry) => {
                    send_edgezero_response(
                        response,
                        request_filter_effects.as_ref(),
                        &SendContext {
                            timings: timings.clone(),
                            server_timing_enabled,
                            method: request_method.clone(),
                            publisher_domain: publisher_domain.clone(),
                            access_sample_rate,
                        },
                    );
                    run_edgezero_pull_sync_after_send(settings, &partner_registry, &ec_state);
                    return;
                }
                Err(e) => {
                    log::error!(
                        "EdgeZero EC finalize skipped: failed to build partner registry: {e:?}"
                    );
                }
            }
        } else {
            match load_settings_from_config_store() {
                Ok(settings) => {
                    match apply_edgezero_ec_finalize(&settings, &ec_state, &mut response, &timings)
                    {
                        Ok(partner_registry) => {
                            send_edgezero_response(
                                response,
                                request_filter_effects.as_ref(),
                                &SendContext {
                                    timings: timings.clone(),
                                    server_timing_enabled,
                                    method: request_method.clone(),
                                    publisher_domain: publisher_domain.clone(),
                                    access_sample_rate,
                                },
                            );
                            run_edgezero_pull_sync_after_send(
                                &settings,
                                &partner_registry,
                                &ec_state,
                            );
                            return;
                        }
                        Err(e) => {
                            log::error!(
                                "EdgeZero EC finalize skipped: failed to build partner registry: {e:?}"
                            );
                        }
                    }
                }
                Err(e) => {
                    log::warn!("EdgeZero EC finalize skipped: failed to reload settings: {e:?}");
                }
            }
        }
    }

    send_edgezero_response(
        response,
        request_filter_effects.as_ref(),
        &SendContext {
            timings,
            server_timing_enabled,
            method: request_method,
            publisher_domain,
            access_sample_rate,
        },
    );
}

fn edge_error_response(error: EdgeError) -> HttpResponse {
    log::error!("EdgeZero router returned error: {error:?}");
    match error.into_response() {
        Ok(response) => response,
        Err(error) => {
            log::error!("failed to convert EdgeZero error into response: {error:?}");
            edgezero_core::http::response_builder()
                .status(edgezero_core::http::StatusCode::INTERNAL_SERVER_ERROR)
                .body(EdgeBody::from("Internal Server Error"))
                .expect("should build EdgeZero error response")
        }
    }
}

fn take_finalize_sentinel(response: &mut HttpResponse) -> bool {
    response
        .headers_mut()
        .remove(HEADER_X_TS_FINALIZED)
        .is_some()
}

fn apply_entry_point_finalize_headers(
    settings: &Settings,
    response: &mut HttpResponse,
    client_ip: Option<std::net::IpAddr>,
    geo_state: &GeoLookupState,
    timings: &RequestTimings,
) {
    let geo_info = resolve_geo_for_response(response, geo_state, client_ip, |client_ip| {
        let _span = timings.span(Phase::Geo);
        FastlyPlatformGeo.lookup(client_ip).unwrap_or_else(|e| {
            log::warn!("entry-point geo lookup failed: {e}");
            None
        })
    });
    apply_finalize_headers(settings, geo_info.as_ref(), response);

    // This path runs only when the middleware chain was bypassed (e.g. a
    // router-level 404/405 for an unregistered method), so `geo_state` may
    // still be `NotAttempted` even after a fresh lookup just ran above.
    // Write the resolved outcome back so the access-telemetry snapshot built
    // later in `send_edgezero_response` sees what was actually looked up,
    // not the stale carried-in state.
    let resolved_state = match &geo_info {
        Some(info) => GeoLookupState::Resolved(info.clone()),
        None => GeoLookupState::Attempted,
    };
    response.extensions_mut().insert(resolved_state);
}

fn apply_edgezero_ec_finalize(
    settings: &Settings,
    ec_state: &EcFinalizeState,
    response: &mut HttpResponse,
    timings: &RequestTimings,
) -> Result<PartnerRegistry, Report<TrustedServerError>> {
    let partner_registry = PartnerRegistry::from_config(&settings.ec.partners)?;
    let finalize_kv_graph = if ec_state.use_finalize_kv {
        identity_graph_with_timing(settings, timings)
    } else {
        None
    };
    ec_finalize_response(
        settings,
        &ec_state.ec_context,
        finalize_kv_graph.as_ref(),
        &partner_registry,
        ec_state.eids_cookie.as_deref(),
        ec_state.sharedid_cookie.as_deref(),
        response,
    );
    Ok(partner_registry)
}

fn run_edgezero_pull_sync_after_send(
    settings: &Settings,
    partner_registry: &PartnerRegistry,
    ec_state: &EcFinalizeState,
) {
    if ec_state.is_real_browser
        && let Some(context) = build_pull_sync_context(&ec_state.ec_context)
    {
        run_pull_sync_after_send(settings, partner_registry, &context, &ec_state.services);
    }
}

/// Per-response context threaded into [`send_edgezero_response`] so the
/// function stays at or under seven parameters.
struct SendContext {
    /// The request's phase-timing collector.
    timings: RequestTimings,
    /// Whether `observability.server_timing_enabled` is set.
    server_timing_enabled: bool,
    /// The request's HTTP method, captured before the request was consumed
    /// by dispatch.
    method: String,
    /// The configured publisher domain.
    publisher_domain: String,
    /// The configured access-telemetry sample rate.
    access_sample_rate: f64,
}

/// Outcome of handing a finalized response to the client.
#[allow(dead_code)]
pub(crate) struct DeliveryOutcome {
    /// Response body size in bytes.
    pub bytes: u64,
    /// Whether delivery completed or failed partway.
    pub result: DeliveryResult,
    /// Access-telemetry dimensions captured for this response at the
    /// freeze point.
    pub snapshot: AccessTelemetrySnapshot,
}

/// Whether [`send_edgezero_response`] completed delivery or failed partway.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DeliveryResult {
    /// The response was handed to the client in full.
    Complete,
    /// Delivery started but did not finish cleanly: some bytes reached the
    /// client's transport before a stream error, or the transport could not
    /// be closed cleanly after every byte was written.
    Partial,
    /// Delivery failed before any bytes reached the client.
    Error,
}

/// Stamps [`RequestTimings::mark_headers_ready`] and, when observability is
/// enabled and the response is conclusively private, appends the rendered
/// `Server-Timing` header.
///
/// Always stamps `mark_headers_ready` regardless of whether the header is
/// rendered, so the collector's `ts-total` reflects the moment headers
/// commit. Appends rather than overwrites so a pre-existing `Server-Timing`
/// value set upstream survives alongside the TS-owned set. A response is
/// never promoted to shared-cacheable just because the header would
/// otherwise be omitted: this only gates emission, it does not touch
/// `Cache-Control`.
pub(crate) fn apply_server_timing_header(
    response: &mut HttpResponse,
    timings: &RequestTimings,
    server_timing_enabled: bool,
) {
    timings.mark_headers_ready();

    let conclusively_private = cache_control_headers_are_private_or_no_store(response.headers());
    if !server_timing_enabled || !conclusively_private {
        return;
    }

    let Some(value) = timings.server_timing_value() else {
        return;
    };
    match HeaderValue::from_str(&value) {
        Ok(header_value) => {
            response
                .headers_mut()
                .append(HEADER_SERVER_TIMING, header_value);
        }
        Err(error) => log::warn!("skipping server-timing header: {error}"),
    }
}

/// A [`Write`](std::io::Write) wrapper that tallies bytes successfully written
/// to the inner writer.
///
/// Wraps the client transport during a streaming drive so a truncated or
/// failed drive still reports how many bytes actually reached it, instead of
/// the placeholder `0` a failed/aborted drive would otherwise report.
struct CountingWriter<W> {
    inner: W,
    bytes: u64,
}

impl<W> CountingWriter<W> {
    fn new(inner: W) -> Self {
        Self { inner, bytes: 0 }
    }

    /// Bytes successfully written to the inner writer so far.
    fn bytes(&self) -> u64 {
        self.bytes
    }

    fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: std::io::Write> std::io::Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.bytes = self.bytes.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Drives a streaming `EdgeZero` body through `output`, tallying bytes written
/// and timing the drive into `timings`.
///
/// Stamps `resp_bytes` and `request_elapsed` immediately once the drive
/// returns — before the caller does anything transport-specific (finishing
/// the streaming body, logging) — so `request_elapsed` never includes that
/// work. Returns the counting writer (so the caller can recover both the
/// tallied byte count and the wrapped transport) alongside the drive's
/// result.
fn drive_streaming_body<W: std::io::Write>(
    body: EdgeBody,
    output: W,
    timings: &RequestTimings,
) -> (CountingWriter<W>, Result<(), Report<TrustedServerError>>) {
    let mut counting = CountingWriter::new(output);
    let drive_started = Instant::now();
    let result = futures::executor::block_on(stream_asset_body(body, &mut counting));
    timings.record(Phase::Stream, drive_started.elapsed());
    timings.set_resp_bytes(counting.bytes());
    timings.mark_request_elapsed();
    (counting, result)
}

/// Classifies a completed streaming drive into a [`DeliveryResult`].
///
/// A drive that failed after writing at least one byte delivered a truncated
/// response rather than nothing at all, so it is [`DeliveryResult::Partial`],
/// not [`DeliveryResult::Error`].
fn classify_stream_delivery(
    drive_result: &Result<(), Report<TrustedServerError>>,
    bytes: u64,
) -> DeliveryResult {
    match drive_result {
        Ok(()) => DeliveryResult::Complete,
        Err(_) if bytes > 0 => DeliveryResult::Partial,
        Err(_) => DeliveryResult::Error,
    }
}

/// Stamps `resp_bytes`/`request_elapsed` for an already-materialized body,
/// immediately before it is handed to the Fastly client transport, and
/// returns its byte length.
fn record_buffered_delivery(body: &EdgeBody, timings: &RequestTimings) -> u64 {
    let bytes = u64::try_from(body.as_bytes().map(<[u8]>::len).unwrap_or(0)).unwrap_or(u64::MAX);
    timings.set_resp_bytes(bytes);
    timings.mark_request_elapsed();
    bytes
}

/// Sends a finalized `EdgeZero` response to the client.
///
/// Streaming `EdgeZero` bodies commit headers first, then pipe chunks to Fastly's
/// client stream so large asset and publisher-origin responses do not
/// materialize in the Wasm heap.
fn send_edgezero_response(
    mut response: HttpResponse,
    request_filter_effects: Option<&RequestFilterEffects>,
    context: &SendContext,
) -> DeliveryOutcome {
    apply_terminal_response_effects(&mut response, request_filter_effects);
    apply_server_timing_header(
        &mut response,
        &context.timings,
        context.server_timing_enabled,
    );

    // Built unconditionally, right after the freeze point and before
    // `into_parts()` consumes `response`: nothing else survives to
    // post-send on every path (the request was consumed by dispatch, and
    // `EcFinalizeState` is absent on asset, admin, and error paths).
    let snapshot = build_access_telemetry_snapshot(&response, context);

    let (parts, body) = response.into_parts();

    match body {
        EdgeBody::Stream(_) => {
            let skeleton = compat::to_fastly_response_skeleton(HttpResponse::from_parts(
                parts,
                EdgeBody::empty(),
            ));
            let (counting, drive_result) =
                drive_streaming_body(body, skeleton.stream_to_client(), &context.timings);
            let bytes = counting.bytes();
            let streaming_body = counting.into_inner();
            // Computed before `drive_result` is matched by value below, since
            // the `Err` arm there moves its `Report` out.
            let result = classify_stream_delivery(&drive_result, bytes);
            match drive_result {
                Ok(()) => match streaming_body.finish() {
                    Ok(()) => DeliveryOutcome {
                        bytes,
                        result: DeliveryResult::Complete,
                        snapshot,
                    },
                    Err(e) => {
                        // Every byte was handed to the transport (the drive
                        // above returned Ok), but the transport itself could
                        // not close cleanly — the client may still see a
                        // truncated response.
                        log::error!("failed to finish EdgeZero streaming body: {e}");
                        DeliveryOutcome {
                            bytes,
                            result: DeliveryResult::Partial,
                            snapshot,
                        }
                    }
                },
                Err(e) => {
                    log::error!("EdgeZero streaming failed: {e:?}");
                    drop(streaming_body);
                    DeliveryOutcome {
                        bytes,
                        result,
                        snapshot,
                    }
                }
            }
        }
        once => {
            let bytes = record_buffered_delivery(&once, &context.timings);
            compat::to_fastly_response(HttpResponse::from_parts(parts, once)).send_to_client();
            DeliveryOutcome {
                bytes,
                result: DeliveryResult::Complete,
                snapshot,
            }
        }
    }
}

/// Builds the [`AccessTelemetrySnapshot`] for `response` at the
/// `Server-Timing` freeze point.
///
/// Reads route identity, geo country, and template-cache state from typed
/// response extensions rather than the headers those extensions back —
/// operator-configured response headers can override a managed header, so
/// reading a header here could silently drift from what actually happened.
/// Falls back to `"unknown"`/[`RouteClass::Other`] sentinels when an
/// extension was never attached (router-generated, asset, and other
/// responses that never passed through a `RouteMetadata`-attaching
/// wrapper).
fn build_access_telemetry_snapshot(
    response: &HttpResponse,
    context: &SendContext,
) -> AccessTelemetrySnapshot {
    let (route_class, route_template) = match response.extensions().get::<RouteMetadata>() {
        Some(metadata) => (metadata.route_class, metadata.route_template.clone()),
        None => (RouteClass::Other, "unknown".to_owned()),
    };

    let country = match response.extensions().get::<GeoLookupState>() {
        Some(GeoLookupState::Resolved(info)) => info.country.clone(),
        Some(GeoLookupState::Attempted | GeoLookupState::NotAttempted) | None => {
            "unknown".to_owned()
        }
    };

    let template_cache_state = response
        .extensions()
        .get::<TemplateCacheResponseState>()
        .map_or_else(|| "unknown".to_owned(), |state| state.as_str().to_owned());

    let body_mode = if matches!(response.body(), EdgeBody::Stream(_)) {
        "streamed"
    } else {
        "buffered"
    };

    AccessTelemetrySnapshot {
        method: context.method.clone(),
        status: response.status().as_u16(),
        route_class,
        route_template,
        publisher_domain: context.publisher_domain.clone(),
        env: resolve_env_dimension(),
        service_id: env_var_or_unknown(ENV_FASTLY_SERVICE_ID),
        pop: env_var_or_unknown(ENV_FASTLY_POP),
        ts_version: env_var_or_unknown(ENV_FASTLY_SERVICE_VERSION),
        country,
        template_cache_state,
        body_mode,
        sample_rate: context.access_sample_rate,
    }
}

/// Derives the `env` access-telemetry dimension from the same
/// `FASTLY_IS_STAGING` input that drives the `x-ts-env` response header
/// (see [`apply_finalize_headers`]), never from [`Settings`] — `Settings`
/// has no environment field and does not gain one for this.
///
/// `"unknown"` covers contexts where the variable is entirely absent (for
/// example native unit tests run outside Fastly Compute); on the Fastly
/// platform the variable is always present, as either `"1"` or not.
fn resolve_env_dimension() -> String {
    match std::env::var(ENV_FASTLY_IS_STAGING) {
        Ok(value) if value == "1" => "staging".to_owned(),
        Ok(_) => "production".to_owned(),
        Err(_) => "unknown".to_owned(),
    }
}

/// Reads a Fastly-provided environment variable, defaulting to `"unknown"`
/// when unset.
fn env_var_or_unknown(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| "unknown".to_owned())
}

/// Apply every late response mutation, then restore privacy invariants before headers commit.
fn apply_terminal_response_effects(
    response: &mut HttpResponse,
    request_filter_effects: Option<&RequestFilterEffects>,
) {
    let must_remain_private = response
        .extensions()
        .get::<TerminalPrivateResponse>()
        .is_some();
    if let Some(effects) = request_filter_effects {
        effects.apply_to_response(response);
    }
    if must_remain_private {
        trusted_server_core::response_privacy::enforce_private_no_store(response);
    }

    // Final cache guards: EC finalization and request-filter effects may have
    // added a per-user Set-Cookie or a private/no-store directive after
    // `apply_finalize_headers` and normalized asset policy reapplication ran.
    crate::middleware::enforce_set_cookie_cache_privacy(response);
    crate::middleware::enforce_uncacheable_cache_privacy(response);
}

const FALLBACK_UNAVAILABLE: &str = "unavailable";
const FALLBACK_NOT_SENT: &str = "not sent";
const FALLBACK_NONE: &str = "none";

// TODO: remove after JA4 evaluation completes - see #645
fn build_ja4_debug_response(req: &FastlyRequest) -> FastlyResponse {
    let ja4 = req.get_tls_ja4().unwrap_or(FALLBACK_UNAVAILABLE);
    let h2 = req
        .get_client_h2_fingerprint()
        .unwrap_or(FALLBACK_UNAVAILABLE);
    let cipher = req
        .get_tls_cipher_openssl_name()
        .ok()
        .flatten()
        .unwrap_or(FALLBACK_UNAVAILABLE);
    let tls_version = req
        .get_tls_protocol()
        .ok()
        .flatten()
        .unwrap_or(FALLBACK_UNAVAILABLE);
    let ua = req.get_header_str("user-agent").unwrap_or(FALLBACK_NONE);
    let ch_mobile = req
        .get_header_str("sec-ch-ua-mobile")
        .unwrap_or(FALLBACK_NOT_SENT);
    let ch_platform = req
        .get_header_str("sec-ch-ua-platform")
        .unwrap_or(FALLBACK_NOT_SENT);

    let body = format!(
        "ja4:         {ja4}\n\
         h2_fp:       {h2}\n\
         cipher:      {cipher}\n\
         tls_version: {tls_version}\n\
         user-agent:  {ua}\n\
         ch-mobile:   {ch_mobile}\n\
         ch-platform: {ch_platform}\n"
    );

    FastlyResponse::from_status(fastly::http::StatusCode::OK)
        .with_header(fastly::http::header::CACHE_CONTROL, "no-store, private")
        .with_header(
            fastly::http::header::VARY,
            "User-Agent, Sec-CH-UA-Mobile, Sec-CH-UA-Platform",
        )
        .with_content_type(fastly::mime::TEXT_PLAIN_UTF_8)
        .with_body(body)
}

/// Constructs a `KvIdentityGraph` wrapped in the [`Phase::EcKv`] timing
/// decorator, for request-path callers with a `RequestTimings` handle.
///
/// Returns `None` when `ec.ec_store` is not configured, matching
/// [`require_identity_graph_with_timing`]'s contract on every other axis.
pub(crate) fn identity_graph_with_timing(
    settings: &Settings,
    timings: &RequestTimings,
) -> Option<KvIdentityGraph> {
    settings.ec.ec_store.as_ref().map(|store_name| {
        KvIdentityGraph::new(TimedKvStore::new(
            FastlyEcKvStore::new(store_name),
            timings.clone(),
        ))
    })
}

fn run_pull_sync_after_send(
    settings: &Settings,
    partner_registry: &PartnerRegistry,
    context: &PullSyncContext,
    services: &RuntimeServices,
) {
    let kv = match require_identity_graph(settings) {
        Ok(kv) => kv,
        Err(err) => {
            log::debug!("Pull sync: identity graph unavailable, skipping: {err:?}");
            return;
        }
    };

    let limiter = FastlyRateLimiter::new(RATE_COUNTER_NAME);
    dispatch_pull_sync(settings, &kv, partner_registry, &limiter, context, services);
}

/// Constructs a `KvIdentityGraph` from settings, or returns an error if the
/// `ec_store` config is not set.
///
/// Deliberately untimed: pull-sync (this function's only caller) runs after
/// `send_edgezero_response`'s Server-Timing freeze point, so a decorated
/// store here would record into a handle nothing ever renders.
/// Request-path callers with a `RequestTimings` handle use
/// [`require_identity_graph_with_timing`] instead.
pub(crate) fn require_identity_graph(
    settings: &Settings,
) -> Result<KvIdentityGraph, Report<TrustedServerError>> {
    let store_name = settings.ec.ec_store.as_deref().ok_or_else(|| {
        Report::new(TrustedServerError::KvStore {
            store_name: "ec.ec_store".to_owned(),
            message: "ec.ec_store is not configured".to_owned(),
        })
    })?;
    Ok(KvIdentityGraph::new(FastlyEcKvStore::new(store_name)))
}

/// Constructs a `KvIdentityGraph` wrapped in the [`Phase::EcKv`] timing
/// decorator, or returns an error if the `ec_store` config is not set.
///
/// Request-path sibling of [`require_identity_graph`], which pull-sync uses
/// unwrapped because pull-sync runs after the Server-Timing freeze point.
pub(crate) fn require_identity_graph_with_timing(
    settings: &Settings,
    timings: &RequestTimings,
) -> Result<KvIdentityGraph, Report<TrustedServerError>> {
    let store_name = settings.ec.ec_store.as_deref().ok_or_else(|| {
        Report::new(TrustedServerError::KvStore {
            store_name: "ec.ec_store".to_owned(),
            message: "ec.ec_store is not configured".to_owned(),
        })
    })?;
    Ok(KvIdentityGraph::new(TimedKvStore::new(
        FastlyEcKvStore::new(store_name),
        timings.clone(),
    )))
}

/// Extracts a named cookie value from the request's `Cookie` header.
pub(crate) fn extract_cookie_value(req: &HttpRequest, name: &str) -> Option<String> {
    let cookie_header = req.headers().get("cookie").and_then(|v| v.to_str().ok())?;
    for pair in cookie_header.split(';') {
        let pair = pair.trim();
        if let Some((key, value)) = pair.split_once('=')
            && key.trim() == name
        {
            return Some(value.trim().to_owned());
        }
    }
    None
}

/// Derives device signals from TLS, H2, and UA request data.
///
/// All extraction is pure in-memory — no KV I/O. The Fastly SDK provides
/// `get_tls_ja4()` and `get_client_h2_fingerprint()` on client requests.
pub(crate) fn derive_device_signals(req: &FastlyRequest) -> DeviceSignals {
    let ua = req.get_header_str("user-agent").unwrap_or("");
    let ja4 = req.get_tls_ja4();
    let h2_fp = req.get_client_h2_fingerprint();

    DeviceSignals::derive(ua, ja4, h2_fp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use edgezero_core::body::Body as EdgeBody;
    use edgezero_core::http::HeaderValue;
    use edgezero_core::http::response_builder;
    use fastly::mime;
    use std::time::Duration;
    use trusted_server_core::integrations::HeaderMutation;
    use trusted_server_core::request_timing::AuctionWaitPlacement;

    fn test_settings() -> Settings {
        Settings::from_toml(
            r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"

            [request_signing]
            enabled = false
            config_store_id = "test-config-store-id"
            secret_store_id = "test-secret-store-id"
            "#,
        )
        .expect("should parse test settings")
    }

    /// A minimal [`AccessTelemetrySnapshot`] fixture for tests that only
    /// need a `DeliveryOutcome` to exist, not its telemetry content.
    fn sample_access_snapshot() -> AccessTelemetrySnapshot {
        AccessTelemetrySnapshot {
            method: "GET".to_owned(),
            status: 200,
            route_class: RouteClass::Other,
            route_template: "/other/*".to_owned(),
            publisher_domain: "unknown".to_owned(),
            env: "unknown".to_owned(),
            service_id: "unknown".to_owned(),
            pop: "unknown".to_owned(),
            ts_version: "unknown".to_owned(),
            country: "unknown".to_owned(),
            template_cache_state: "unknown".to_owned(),
            body_mode: "buffered",
            sample_rate: 0.0,
        }
    }

    #[test]
    fn health_response_short_circuits_get_health() {
        let req = FastlyRequest::get("https://example.com/health");

        let mut response = health_response(&req).expect("should build health response");

        assert_eq!(
            response.get_status(),
            fastly::http::StatusCode::OK,
            "should return 200 OK"
        );
        assert_eq!(
            response.take_body_str(),
            "ok",
            "should return the health body"
        );
    }

    #[test]
    fn health_response_ignores_non_health_paths() {
        let req = FastlyRequest::get("https://example.com/auction");

        assert!(
            health_response(&req).is_none(),
            "should only short-circuit /health"
        );
    }

    #[test]
    fn take_finalize_sentinel_strips_sentinel() {
        let mut response = HttpResponse::new(EdgeBody::empty());
        response
            .headers_mut()
            .insert("x-ts-finalized", HeaderValue::from_static("1"));

        assert!(
            take_finalize_sentinel(&mut response),
            "should detect middleware-finalized responses"
        );
        assert!(
            response.headers().get("x-ts-finalized").is_none(),
            "sentinel should not be sent to clients"
        );
    }

    #[test]
    fn late_filter_effects_cannot_make_an_assembled_response_public() {
        let mut response = response_builder()
            .header("cache-control", "private, no-store")
            .header("etag", "\"reader-document\"")
            .body(EdgeBody::empty())
            .expect("should build response");
        response.extensions_mut().insert(TerminalPrivateResponse);
        let effects = RequestFilterEffects {
            request_headers: Vec::new(),
            response_headers: vec![
                HeaderMutation::set("cache-control", "public, s-maxage=3600"),
                HeaderMutation::set("surrogate-control", "max-age=3600"),
                HeaderMutation::set("cdn-cache-control", "public, max-age=3600"),
            ],
        };

        apply_terminal_response_effects(&mut response, Some(&effects));

        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|value| value.to_str().ok()),
            Some("no-store, private")
        );
        assert!(response.headers().get("surrogate-control").is_none());
        assert!(response.headers().get("cdn-cache-control").is_none());
        assert!(response.headers().get("etag").is_none());
    }

    #[test]
    fn late_filter_effects_cannot_make_a_page_bids_response_public() {
        let mut response = trusted_server_core::publisher::page_bids_preflight_denied();
        let effects = RequestFilterEffects {
            request_headers: Vec::new(),
            response_headers: vec![
                HeaderMutation::set("cache-control", "public, s-maxage=3600"),
                HeaderMutation::set("surrogate-control", "max-age=3600"),
                HeaderMutation::set("cdn-cache-control", "public, max-age=3600"),
            ],
        };

        apply_terminal_response_effects(&mut response, Some(&effects));

        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|value| value.to_str().ok()),
            Some("no-store, private")
        );
        assert!(response.headers().get("surrogate-control").is_none());
        assert!(response.headers().get("cdn-cache-control").is_none());
    }

    fn diagnostics_settings() -> Settings {
        Settings::from_toml(
            r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"

            [request_signing]
            enabled = false
            config_store_id = "test-config-store-id"
            secret_store_id = "test-secret-store-id"

            [integrations.gpt_diagnostics]
            enabled = true
            "#,
        )
        .expect("should parse diagnostics settings")
    }

    #[test]
    fn late_filter_effects_cannot_make_an_active_diagnostics_response_public() {
        // The narrowest hole: an established diagnostics session sets no new cookie, so
        // the `Set-Cookie` privacy net never fires, and before this the decision only
        // stamped `Cache-Control` without leaving a marker for the terminal guard.
        let mut request = edgezero_core::http::request_builder()
            .method(fastly::http::Method::GET)
            .uri("https://test-publisher.com/article")
            .header("sec-fetch-dest", "document")
            .header("cookie", "__Host-ts-console=1")
            .body(EdgeBody::empty())
            .expect("should build request");
        let decision = trusted_server_core::integrations::gpt_diagnostics::prepare_request(
            &diagnostics_settings(),
            &mut request,
        )
        .expect("should prepare the diagnostics decision");
        assert!(
            decision.active(),
            "the session cookie should activate diagnostics"
        );

        let mut response = response_builder()
            .header("cache-control", "public, max-age=600")
            .body(EdgeBody::empty())
            .expect("should build response");
        trusted_server_core::integrations::gpt_diagnostics::finalize_response(
            &decision,
            &mut response,
        );

        let effects = RequestFilterEffects {
            request_headers: Vec::new(),
            response_headers: vec![
                HeaderMutation::set("cache-control", "public, s-maxage=3600"),
                HeaderMutation::set("surrogate-control", "max-age=3600"),
            ],
        };

        apply_terminal_response_effects(&mut response, Some(&effects));

        assert!(
            response.headers().get("set-cookie").is_none(),
            "the case under test is the one with no Set-Cookie to protect it"
        );
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|value| value.to_str().ok()),
            Some("no-store, private"),
            "request-scoped diagnostics HTML must never become shared-cacheable"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "should strip CDN cache directives a late filter added"
        );
    }

    #[test]
    fn terminal_response_preserves_unmarked_origin_private_policy() {
        let mut response = response_builder()
            .header("cache-control", "private, max-age=600")
            .header("etag", "\"origin\"")
            .header("last-modified", "Wed, 12 Aug 2026 00:00:00 GMT")
            .body(EdgeBody::empty())
            .expect("should build response");

        apply_terminal_response_effects(&mut response, None);

        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|value| value.to_str().ok()),
            Some("private, max-age=600"),
            "should preserve the origin browser-cache policy"
        );
        assert_eq!(
            response
                .headers()
                .get("etag")
                .and_then(|value| value.to_str().ok()),
            Some("\"origin\""),
            "should preserve the origin validator"
        );
        assert_eq!(
            response
                .headers()
                .get("last-modified")
                .and_then(|value| value.to_str().ok()),
            Some("Wed, 12 Aug 2026 00:00:00 GMT"),
            "should preserve the origin modification date"
        );
    }

    #[test]
    #[allow(clippy::panic)]
    fn entry_point_finalize_skips_geo_lookup_for_401() {
        let settings = test_settings();
        let mut response = response_builder()
            .status(edgezero_core::http::StatusCode::UNAUTHORIZED)
            .body(EdgeBody::empty())
            .expect("should build response");

        let geo_info =
            resolve_geo_for_response(&response, &GeoLookupState::NotAttempted, None, |_| {
                panic!("should skip entry-point geo lookup for 401 responses");
            });
        apply_finalize_headers(&settings, geo_info.as_ref(), &mut response);

        assert_eq!(
            response
                .headers()
                .get(trusted_server_core::constants::HEADER_X_GEO_INFO_AVAILABLE)
                .and_then(|v| v.to_str().ok()),
            Some("false"),
            "401 responses should still carry geo-unavailable headers"
        );
    }

    #[test]
    fn ja4_debug_response_uses_plain_text_and_fallback_values() {
        let req = FastlyRequest::get("https://example.com/_ts/debug/ja4");

        let mut response = build_ja4_debug_response(&req);

        assert_eq!(
            response.get_status(),
            fastly::http::StatusCode::OK,
            "should return 200 OK"
        );
        assert_eq!(
            response.get_content_type(),
            Some(mime::TEXT_PLAIN_UTF_8),
            "should return plain text content"
        );
        assert_eq!(
            response.get_header_str(fastly::http::header::CACHE_CONTROL),
            Some("no-store, private"),
            "should disable caching for the debug response"
        );

        let body = response.take_body_str();

        assert!(
            body.contains("ja4:         unavailable"),
            "should include JA4 fallback"
        );
        assert!(
            body.contains("h2_fp:       unavailable"),
            "should include H2 probabilistic identifier fallback"
        );
        assert!(
            body.contains("cipher:      unavailable"),
            "should include cipher fallback"
        );
        assert!(
            body.contains("tls_version: unavailable"),
            "should include TLS version fallback"
        );
        assert!(
            body.contains("user-agent:  none"),
            "should include user-agent fallback"
        );
        assert!(
            body.contains("ch-mobile:   not sent"),
            "should include sec-ch-ua-mobile fallback"
        );
        assert!(
            body.contains("ch-platform: not sent"),
            "should include sec-ch-ua-platform fallback"
        );
    }

    fn ec_finalize_settings() -> Settings {
        Settings::from_toml(
            r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"
            ec_store = "ec_identity_store"

            [[ec.partners]]
            name = "Example Partner"
            source_domain = "example.com"
            api_token = "test-vendor-token-32-bytes-minimum"

            [request_signing]
            enabled = false
            config_store_id = "test-config-store-id"
            secret_store_id = "test-secret-store-id"
            "#,
        )
        .expect("should parse EC finalize test settings")
    }

    /// Minimal `RuntimeServices` for `EcFinalizeState.services`. Real
    /// `FastlyPlatform*` handles are used as inert placeholders: EC
    /// finalization never calls through them, it only satisfies the field.
    fn inert_runtime_services() -> RuntimeServices {
        RuntimeServices::builder()
            .config_store(Arc::new(crate::platform::FastlyPlatformConfigStore))
            .secret_store(Arc::new(crate::platform::FastlyPlatformSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore)
                as Arc<dyn trusted_server_core::platform::PlatformKvStore>)
            .backend(Arc::new(crate::platform::FastlyPlatformBackend))
            .http_client(Arc::new(crate::platform::FastlyPlatformHttpClient))
            .geo(Arc::new(crate::platform::FastlyPlatformGeo))
            .client_info(trusted_server_core::platform::ClientInfo::default())
            .build()
    }

    #[test]
    fn ec_finalize_kv_lands_before_freeze() {
        // A pre-seeded EC entry (see fastly.toml's ec_identity_store fixture)
        // for a returning user carrying an eids cookie that matches the
        // configured partner. This drives ec_finalize_response into
        // ingest_eid_cookies, which reads and writes the KV identity graph.
        let settings = ec_finalize_settings();
        let ec_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.test01";
        let eids = serde_json::json!([{
            "source": "example.com",
            "uids": [{ "id": "example-uid", "atype": 1 }]
        }]);
        let eids_cookie = base64::engine::general_purpose::STANDARD.encode(eids.to_string());
        let request = edgezero_core::http::request_builder()
            .method(fastly::http::Method::GET)
            .uri("https://test-publisher.com/article")
            .header("cookie", format!("ts-ec={ec_id}; ts-eids={eids_cookie}"))
            .body(EdgeBody::empty())
            .expect("should build EC finalize test request");

        let services = inert_runtime_services();
        let geo_info = trusted_server_core::platform::GeoInfo {
            city: String::new(),
            country: "US".to_owned(),
            continent: "NorthAmerica".to_owned(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: None,
            asn: None,
        };
        let ec_context = trusted_server_core::ec::EcContext::read_from_request_with_geo(
            &settings,
            &request,
            &services,
            Some(&geo_info),
        )
        .expect("should read EC context from a non-regulated request");
        assert!(
            ec_context.ec_was_present(),
            "the pre-seeded ts-ec cookie should be recognized"
        );

        let ec_state = EcFinalizeState {
            ec_context,
            use_finalize_kv: true,
            eids_cookie: Some(eids_cookie),
            sharedid_cookie: None,
            is_real_browser: true,
            services,
        };
        let mut response = response_builder()
            .header("cache-control", "private, no-store")
            .body(EdgeBody::empty())
            .expect("should build EC finalize response fixture");
        let timings = RequestTimings::new();

        // Mirrors edgezero_main's ordering: EC finalize runs, then the freeze
        // point (apply_server_timing_header, called just before
        // response.into_parts() inside send_edgezero_response) renders the
        // header. Calling both directly exercises exactly this order without
        // requiring a live Fastly client connection.
        apply_edgezero_ec_finalize(&settings, &ec_state, &mut response, &timings)
            .expect("should finalize EC response");
        apply_server_timing_header(&mut response, &timings, true);

        let header = response
            .headers()
            .get("server-timing")
            .and_then(|v| v.to_str().ok())
            .expect("should emit a Server-Timing header");
        assert!(
            header.contains("ts-kv"),
            "the freeze point must run after EC finalization recorded KV time: {header}"
        );
    }

    #[test]
    fn delivery_outcome_reports_bytes_and_request_elapsed_set() {
        let timings = RequestTimings::new();
        let body = EdgeBody::stream(futures::stream::iter(vec![
            bytes::Bytes::from_static(b"hello "),
            bytes::Bytes::from_static(b"world"),
        ]));

        let (counting, drive_result) = drive_streaming_body(body, Vec::new(), &timings);
        drive_result.expect("streaming a well-formed body should not fail");
        let bytes = counting.bytes();
        let outcome = DeliveryOutcome {
            bytes,
            result: DeliveryResult::Complete,
            snapshot: sample_access_snapshot(),
        };

        assert_eq!(
            counting.into_inner(),
            b"hello world",
            "should write every byte to the underlying transport"
        );
        assert_eq!(
            outcome.bytes,
            "hello world".len() as u64,
            "DeliveryOutcome.bytes should equal the streamed body length"
        );

        let snapshot = timings.snapshot();
        assert_eq!(
            snapshot.resp_bytes,
            Some("hello world".len() as u64),
            "should stamp resp_bytes to the tallied byte count"
        );
        assert!(
            snapshot.request_elapsed_ms.is_some(),
            "should stamp request_elapsed once the drive returns"
        );
    }

    #[test]
    fn buffered_delivery_stamps_bytes_and_request_elapsed() {
        let timings = RequestTimings::new();
        let body = EdgeBody::from(b"a buffered body".to_vec());

        let bytes = record_buffered_delivery(&body, &timings);

        assert_eq!(
            bytes,
            "a buffered body".len() as u64,
            "should report the buffered body length"
        );
        let snapshot = timings.snapshot();
        assert_eq!(
            snapshot.resp_bytes,
            Some("a buffered body".len() as u64),
            "should stamp resp_bytes for the buffered path too"
        );
        assert!(
            snapshot.request_elapsed_ms.is_some(),
            "should stamp request_elapsed for the buffered path too"
        );
    }

    fn send_context_fixture() -> SendContext {
        SendContext {
            timings: RequestTimings::new(),
            server_timing_enabled: false,
            method: "GET".to_owned(),
            publisher_domain: "test-publisher.com".to_owned(),
            access_sample_rate: 0.25,
        }
    }

    #[test]
    fn access_snapshot_defaults_when_no_extensions_are_attached() {
        // Router-generated 404/405 responses and other paths that never pass
        // through a RouteMetadata-attaching wrapper must still produce a
        // usable snapshot: RouteClass::Other and "unknown" sentinels, never
        // a missing/panicking build.
        let response = response_builder()
            .status(404)
            .body(EdgeBody::empty())
            .expect("should build response");
        let context = send_context_fixture();

        let snapshot = build_access_telemetry_snapshot(&response, &context);

        assert_eq!(snapshot.status, 404);
        assert_eq!(snapshot.method, "GET");
        assert!(matches!(snapshot.route_class, RouteClass::Other));
        assert_eq!(snapshot.route_template, "unknown");
        assert_eq!(snapshot.country, "unknown");
        assert_eq!(snapshot.template_cache_state, "unknown");
        assert_eq!(snapshot.body_mode, "buffered");
        assert_eq!(snapshot.publisher_domain, "test-publisher.com");
        assert_eq!(snapshot.sample_rate, 0.25);
    }

    #[test]
    fn access_snapshot_reads_route_geo_and_template_cache_extensions() {
        let mut response = response_builder()
            .status(200)
            .body(EdgeBody::empty())
            .expect("should build response");
        response.extensions_mut().insert(RouteMetadata {
            route_class: RouteClass::AuctionApi,
            route_template: "/auction".to_owned(),
        });
        response.extensions_mut().insert(GeoLookupState::Resolved(
            trusted_server_core::platform::GeoInfo {
                city: String::new(),
                country: "US".to_owned(),
                continent: "NorthAmerica".to_owned(),
                latitude: 0.0,
                longitude: 0.0,
                metro_code: 0,
                region: None,
                asn: None,
            },
        ));
        response
            .extensions_mut()
            .insert(TemplateCacheResponseState::Hit);
        let context = send_context_fixture();

        let snapshot = build_access_telemetry_snapshot(&response, &context);

        assert!(matches!(snapshot.route_class, RouteClass::AuctionApi));
        assert_eq!(snapshot.route_template, "/auction");
        assert_eq!(snapshot.country, "US");
        assert_eq!(snapshot.template_cache_state, "hit");
    }

    #[test]
    fn access_snapshot_treats_attempted_geo_lookup_as_unknown_country() {
        let mut response = response_builder()
            .status(200)
            .body(EdgeBody::empty())
            .expect("should build response");
        response.extensions_mut().insert(GeoLookupState::Attempted);
        let context = send_context_fixture();

        let snapshot = build_access_telemetry_snapshot(&response, &context);

        assert_eq!(
            snapshot.country, "unknown",
            "an attempted-but-unresolved lookup must not surface a stale country"
        );
    }

    #[test]
    fn access_snapshot_body_mode_reflects_the_response_body_variant() {
        let streamed = response_builder()
            .status(200)
            .body(EdgeBody::stream(futures::stream::empty()))
            .expect("should build streaming response");
        let buffered = response_builder()
            .status(200)
            .body(EdgeBody::from(b"hi".to_vec()))
            .expect("should build buffered response");
        let context = send_context_fixture();

        assert_eq!(
            build_access_telemetry_snapshot(&streamed, &context).body_mode,
            "streamed"
        );
        assert_eq!(
            build_access_telemetry_snapshot(&buffered, &context).body_mode,
            "buffered"
        );
    }

    #[test]
    fn stream_drive_records_stream_ms_covering_the_in_stream_auction_wait() {
        // A streaming seam wait (Task 6, publisher.rs) records into the same
        // `RequestTimings` handle the adapter drives with. `Phase::Stream`
        // wraps the entire drive, so it must cover — and therefore be at
        // least as large as — any `AuctionWait` recorded while the body was
        // being polled.
        let timings = RequestTimings::new();
        let wait_timings = timings.clone();
        let stream = futures::stream::once(async move {
            let waited = Duration::from_millis(5);
            std::thread::sleep(waited);
            wait_timings.record_auction_wait(AuctionWaitPlacement::InStream, waited);
            bytes::Bytes::from_static(b"<html></html>")
        });
        let body = EdgeBody::stream(stream);

        let (_counting, drive_result) = drive_streaming_body(body, Vec::new(), &timings);
        drive_result.expect("streaming a well-formed body should not fail");

        let snapshot = timings.snapshot();
        assert_eq!(
            snapshot.auction_wait_placement,
            Some(AuctionWaitPlacement::InStream),
            "should preserve the placement recorded from inside the polled body"
        );
        let auction_wait_ms = snapshot
            .auction_wait_ms
            .expect("should record the auction wait");
        let stream_ms = snapshot.stream_ms.expect("should record the stream drive");
        assert!(
            stream_ms >= auction_wait_ms,
            "the drive's Phase::Stream span must cover the in-stream auction wait: \
             stream_ms={stream_ms} auction_wait_ms={auction_wait_ms}"
        );
    }

    #[test]
    fn classify_stream_delivery_treats_bytes_written_before_an_error_as_partial() {
        let err = Report::new(TrustedServerError::Proxy {
            message: "boom".to_string(),
        });
        assert_eq!(
            classify_stream_delivery(&Err(err), 42),
            DeliveryResult::Partial,
            "bytes already on the wire before a stream error is a truncated delivery"
        );
    }

    #[test]
    fn classify_stream_delivery_treats_an_error_with_no_bytes_as_error() {
        let err = Report::new(TrustedServerError::Proxy {
            message: "boom".to_string(),
        });
        assert_eq!(
            classify_stream_delivery(&Err(err), 0),
            DeliveryResult::Error,
            "a failure before any byte reached the client is a clean failure, not a truncation"
        );
    }

    #[test]
    fn classify_stream_delivery_treats_ok_as_complete() {
        assert_eq!(
            classify_stream_delivery(&Ok(()), 123),
            DeliveryResult::Complete
        );
    }
}
