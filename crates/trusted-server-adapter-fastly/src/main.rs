use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use edgezero_adapter_fastly::config_store::FastlyConfigStore as EdgeZeroFastlyConfigStore;
use edgezero_adapter_fastly::request::into_core_request;
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::config_store::ConfigStoreHandle;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{Request as HttpRequest, Response as HttpResponse};
use edgezero_core::response::IntoResponse;
use error_stack::Report;
use fastly::http::Method as FastlyMethod;
use fastly::{Request as FastlyRequest, Response as FastlyResponse};
use rand::Rng as _;

use trusted_server_core::access_telemetry::{
    AccessTelemetrySnapshot, DeliveryResult, RouteClass, RouteMetadata, access_event_row,
};
use trusted_server_core::cache_policy::{EdgeCacheHeader, cache_control_headers_have_directive};
use trusted_server_core::constants::{
    ENV_FASTLY_IS_STAGING, ENV_FASTLY_POP, ENV_FASTLY_SERVICE_ID, HEADER_X_TS_VERSION,
};
use trusted_server_core::ec::EcKvSnapshot;
use trusted_server_core::ec::device::DeviceSignals;
use trusted_server_core::ec::finalize::{ec_finalize_response, retry_failed_withdrawals};
use trusted_server_core::ec::identity::IdentityRuntime;
use trusted_server_core::ec::identity::flush::{
    AfterSendInput, FlushOutcome, defer_capture_writes, lose_pending_removals, may_flush,
    retry_pending_removals, run_after_send,
};
use trusted_server_core::ec::identity::measurement::{IdentityMeasurement, MeasurementInput};
use trusted_server_core::ec::kv::KvIdentityGraph;
use trusted_server_core::ec::kv_backend::KvOperationCounts;
use trusted_server_core::ec::rate_limiter::RateLimiter;
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::geo::GeoLookupState;
use trusted_server_core::integrations::RequestFilterEffects;
use trusted_server_core::platform::PlatformGeo as _;
use trusted_server_core::platform::PlatformHttpClient;
use trusted_server_core::proxy::{AssetProxyCachePolicy, stream_asset_body};
use trusted_server_core::publisher::TemplateCacheResponseState;
use trusted_server_core::request_timing::{Phase, RequestTimings, append_server_timing_if_private};
use trusted_server_core::response_privacy::TerminalPrivateResponse;
use trusted_server_core::settings::Settings;
use trusted_server_core::trace::{TraceTerminalResponse, is_trace_path};
use trusted_server_core::version_header::{apply_git_version_header, git_version_header_value};

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
mod sandbox;
mod template_cache;
mod tinybird;

use crate::app::{
    AppState, EcFinalizeState, RuntimeStoreConfig, TrustedServerApp,
    build_identity_runtime_from_reloaded_settings, load_settings_from_config_store,
};
use crate::ec_kv::FastlyEcKvStore;
use crate::middleware::{HEADER_X_TS_FINALIZED, apply_finalize_headers, resolve_geo_for_response};
use crate::platform::{FastlyPlatformGeo, client_info_from_request, resolve_client_ip};
use crate::rate_limiter::{FastlyRateLimiter, RATE_COUNTER_NAME};
use crate::sandbox::{RetainedApp, Sandbox, SandboxCounters, ServeMode, StartupDiagnostics};
// Only the reuse path builds a serving loop, so the retirement snapshots have
// no consumer in the default build.
#[cfg(feature = "reusable-sandbox")]
use crate::sandbox::RetirementCounters;

/// Opens the Fastly Config Store used by the `EdgeZero` dispatcher.
///
/// # Errors
///
/// Returns [`fastly::Error`] if the config store cannot be opened.
fn open_trusted_server_config_store(store_name: &str) -> Result<ConfigStoreHandle, fastly::Error> {
    let store = EdgeZeroFastlyConfigStore::try_open(store_name).map_err(|e| {
        fastly::Error::msg(format!("failed to open config store `{store_name}`: {e}"))
    })?;
    Ok(ConfigStoreHandle::new(Arc::new(store)))
}

fn health_response(req: &FastlyRequest) -> Option<FastlyResponse> {
    if req.get_method() == FastlyMethod::GET && req.get_path() == "/health" {
        let mut response = FastlyResponse::from_status(200).with_body_text_plain("ok");
        // Compiled-in constant: keeps the probe free of settings and app construction.
        if let Some(version) = git_version_header_value() {
            response.set_header(HEADER_X_TS_VERSION, version);
        }
        return Some(response);
    }

    None
}

/// Entry point for the Fastly Compute program.
///
/// Uses an undecorated `main()` with `FastlyRequest::from_client()` instead of
/// `#[fastly::main]` so the `EdgeZero` streaming publisher path can call
/// [`fastly::Response::stream_to_client`] explicitly. It owns the sandbox
/// lifecycle and delegates each request to [`handle_request`].
///
/// Without the `reusable-sandbox` feature this takes one request and returns,
/// which is the original behaviour. With the feature, the bounds still have to
/// come from the runtime environment before the SDK serving loop is entered;
/// an unconfigured or partially configured sandbox stays single-request.
fn main() {
    let (mode, diagnostics) = serve_mode();

    // Held outside the sandbox: `serve_custom` owns the `Sandbox` and exposes
    // no slot for application state that is not the retained payload.
    let mut startup = StartupDiagnostics::default();
    for message in diagnostics {
        startup.push(message);
    }

    match mode {
        ServeMode::Single => {
            // `run_custom` completes the callback's SDK result exactly once.
            // The callback sends its own response and returns `()`, so there
            // is no error for the SDK to turn into a second response.
            let Ok(()) = edgezero_adapter_fastly::lifecycle::run_custom(
                FastlyRequest::from_client(),
                |request, sandbox: &mut Sandbox| handle_request(request, sandbox, &mut startup),
            );
        }
        ServeMode::Reuse(limits) => serve_loop(limits, startup),
    }
}

/// Serves requests from one sandbox under explicit bounds.
///
/// Only compiled with the `reusable-sandbox` feature; [`serve_mode`] can never
/// return [`ServeMode::Reuse`] without it.
#[cfg(feature = "reusable-sandbox")]
fn serve_loop(limits: crate::sandbox::SandboxLimits, mut startup: StartupDiagnostics) {
    // `serve_custom` owns the `Sandbox` and drops it when serving ends, so the
    // retirement line reports snapshots taken while it was still borrowed.
    // `RetirementCounters` reads the attempt count on the way out, so a build
    // performed by the final callback is included.
    let mut counters = RetirementCounters::default();

    let summary = edgezero_adapter_fastly::lifecycle::serve_custom(
        fastly::http::serve::Serve::new()
            .with_max_requests(limits.max_requests)
            .with_max_memory(limits.max_memory_mib)
            .with_max_lifetime(limits.max_lifetime)
            .with_timeout(limits.timeout),
        |request, sandbox: &mut Sandbox| {
            counters.observe(sandbox, |sandbox| {
                handle_request(request, sandbox, &mut startup);
            });
        },
    );

    log::info!(
        "sandbox retiring after {} attempted callback(s), {} observed, {} build attempt(s)",
        summary.requests(),
        counters.requests(),
        counters.attempts()
    );
}

#[cfg(not(feature = "reusable-sandbox"))]
fn serve_loop(_limits: crate::sandbox::SandboxLimits, _startup: StartupDiagnostics) {
    unreachable!("serve_mode never selects reuse without the reusable-sandbox feature")
}

/// Handles one request end to end, sending its own response.
///
/// Returns `()` rather than a response so the streaming publisher path can
/// call [`fastly::Response::stream_to_client`] itself. The SDK's
/// `HandlerResult` impl for `()` treats that as already sent.
///
/// Every non-panicking path through this function must send exactly once. In a
/// reused sandbox the SDK refuses to wait for the next request until the
/// current one is complete, so a missed send stalls the loop rather than
/// merely dropping one response.
fn handle_request(req: FastlyRequest, sandbox: &mut Sandbox, startup: &mut StartupDiagnostics) {
    // The framework counts the callback before invoking it, including early
    // returns, so this is already this request's 1-based ordinal.
    let ordinal = sandbox.requests();
    // Classified before any native mutation, and before the health probe, so
    // trace-path decisions describe the request as received.
    let trace_path = is_trace_path(req.get_path());

    // Health probe bypasses logging, settings, and app construction as a cheap liveness signal.
    if !trace_path && let Some(response) = health_response(&req) {
        response.send_to_client();
        return;
    }

    // Marked complete only once installation succeeds, so a failed install is
    // retried on a later callback. `setup_once` rolls nothing back, so that is
    // only correct because `init_logger` is harmless to repeat — see its docs.
    match sandbox.setup_once(crate::logging::init_logger) {
        Ok(()) => startup.flush(),
        Err(error) => {
            // Logger installation failed, so its own error cannot rely on log.
            // Keep startup diagnostics pending until installation succeeds.
            #[allow(clippy::print_stderr, reason = "logger installation failed")]
            {
                eprintln!("logger installation failed, retrying next callback: {error}");
            }
        }
    }

    // Correlation is request-local and never retained. `FASTLY_TRACE_ID` names
    // the sandbox, not the request, so it is not used here. The id rides on the
    // response only when metrics are enabled; the client request is left
    // untouched so nothing new reaches origin in the default configuration.
    let request_id = req
        .get_client_request_id()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}-{ordinal}", instance_id()));

    edgezero_main(req, sandbox, ordinal, &request_id, trace_path);
}

/// Builds the counters snapshot response.
///
/// A snapshot only. It reports the sandbox that served *this* probe, which is
/// not necessarily the sandbox that served any preceding workload request, so
/// reuse is established from the counters attached to workload responses
/// rather than from polling this.
///
/// `ordinal` is this probe's own 1-based position in the sandbox, which is
/// also the number of callbacks the sandbox has served including this one.
/// The lifetime count is therefore not reported separately.
#[cfg(feature = "reusable-sandbox")]
fn sandbox_metrics_response(sandbox: &Sandbox, ordinal: u64) -> FastlyResponse {
    let body = serde_json::json!({
        "instance": instance_id(),
        "ordinal": ordinal,
        "builds": sandbox.initialization_attempts(),
    });

    FastlyResponse::from_status(fastly::http::StatusCode::OK)
        .with_header("cache-control", "private, no-store")
        .with_body_json(&body)
        .unwrap_or_else(|e| {
            log::error!("failed to serialize sandbox metrics: {e}");
            FastlyResponse::from_status(fastly::http::StatusCode::INTERNAL_SERVER_ERROR)
        })
}

/// Attaches sandbox counters only to private, no-store workload responses.
///
/// Called before headers are committed, which on the streaming path means
/// before `stream_to_client`. The counters therefore describe the request up
/// to commitment and cannot report its eventual outcome; a failure after
/// commitment is recorded in logs instead and reconciled during analysis.
fn attach_sandbox_counters(response: &mut HttpResponse, counters: &SandboxCounters) {
    // Per-request measurements must never be replayed from a cache. Requiring
    // no-store also excludes browser-cacheable and field-qualified privacy.
    if !cache_control_headers_have_directive(response.headers(), "private")
        || !cache_control_headers_have_directive(response.headers(), "no-store")
    {
        return;
    }

    let headers = response.headers_mut();
    for (name, value) in [
        (sandbox::HEADER_SANDBOX_INSTANCE, instance_id()),
        (
            sandbox::HEADER_SANDBOX_ORDINAL,
            counters.ordinal.to_string(),
        ),
        (sandbox::HEADER_SANDBOX_BUILDS, counters.builds.to_string()),
        (
            sandbox::HEADER_SANDBOX_REQUEST_ID,
            counters.request_id.clone(),
        ),
        (sandbox::HEADER_SANDBOX_VCPU_MS, vcpu_ms()),
        (sandbox::HEADER_SANDBOX_HEAP_MIB, heap_mib()),
    ] {
        match edgezero_core::http::HeaderValue::from_str(&value) {
            Ok(value) => {
                headers.insert(name, value);
            }
            Err(e) => log::warn!("sandbox counter `{name}` is not a valid header value: {e}"),
        }
    }
}

/// Resolves how this sandbox will serve requests.
///
/// Without the `reusable-sandbox` feature this is unconditionally
/// [`ServeMode::Single`] and reads nothing, so the default build does no
/// startup work the original entry point did not do.
/// Returns the mode alongside diagnostics that must wait for the logger.
///
/// This runs before any logger exists, so the reasons reuse was declined are
/// carried out rather than logged here, where they would be discarded.
#[cfg(feature = "reusable-sandbox")]
fn serve_mode() -> (ServeMode, Vec<String>) {
    let (raw, diagnostics) = crate::sandbox::read_raw_limits();
    (crate::sandbox::resolve_mode(raw), diagnostics)
}

#[cfg(not(feature = "reusable-sandbox"))]
fn serve_mode() -> (ServeMode, Vec<String>) {
    (ServeMode::Single, Vec::new())
}

/// Guest-instance identifier used to attribute requests to a sandbox.
///
/// `FASTLY_TRACE_ID` describes the sandbox, which is exactly what is wanted
/// here and exactly why it must not be used as a request id. An absent value
/// is reported rather than synthesized, so a measurement run cannot silently
/// claim reuse it never observed.
fn instance_id() -> String {
    // The SDK documents this as the per-sandbox identifier; on wasm32-wasip1
    // it resolves to `FASTLY_TRACE_ID`, which is why that value must never be
    // used as a request id.
    let id = fastly::compute_runtime::sandbox_id();
    if id.is_empty() {
        return sandbox::INSTANCE_ID_UNAVAILABLE.to_owned();
    }
    id.to_owned()
}

/// Cumulative guest vCPU milliseconds, or a marker when unsupported.
fn vcpu_ms() -> String {
    fastly::compute_runtime::elapsed_vcpu_ms().map_or_else(
        |_| sandbox::COUNTER_UNSUPPORTED.to_owned(),
        |v| v.to_string(),
    )
}

/// Guest heap snapshot in MiB, or a marker when unsupported.
fn heap_mib() -> String {
    fastly::compute_runtime::heap_memory_snapshot_mib().map_or_else(
        |_| sandbox::COUNTER_UNSUPPORTED.to_owned(),
        |v| v.to_string(),
    )
}

/// Handles a request through the `EdgeZero` router path.
fn edgezero_main(
    mut req: FastlyRequest,
    sandbox: &mut Sandbox,
    ordinal: u64,
    request_id: &str,
    trace_path: bool,
) {
    let runtime_stores = RuntimeStoreConfig::logical();

    // Short-circuit the sandbox counters probe before app construction. It must
    // not build the application: polling it would otherwise increment the very
    // build counter it reports.
    #[cfg(feature = "reusable-sandbox")]
    if req.get_method() == FastlyMethod::GET && req.get_path() == sandbox::SANDBOX_METRICS_PATH {
        match load_settings_from_config_store(&runtime_stores) {
            Ok(settings) if sandbox::metrics_enabled(&settings) => {
                sandbox_metrics_response(sandbox, ordinal).send_to_client();
            }
            Ok(_) => {
                FastlyResponse::from_status(fastly::http::StatusCode::NOT_FOUND).send_to_client();
            }
            Err(e) => {
                log::warn!("sandbox metrics endpoint: failed to load settings: {e:?}");
                FastlyResponse::from_status(fastly::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_body_text_plain("Internal Server Error")
                    .send_to_client();
            }
        }
        return;
    }

    // Short-circuit the JA4 debug probe before app construction. Must run here
    // because TLS/JA4 accessors are only available on FastlyRequest before
    // conversion to edgezero types.
    if !trace_path && req.get_method() == FastlyMethod::GET && req.get_path() == "/_ts/debug/ja4" {
        match load_settings_from_config_store(&runtime_stores) {
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

    let config_store = {
        let _appbuild = timings.span(Phase::AppBuild);
        match open_trusted_server_config_store(runtime_stores.config_store_name.as_ref()) {
            Ok(cs) => cs,
            Err(e) => {
                log::error!("failed to open config store: {e}");
                FastlyResponse::from_status(fastly::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_body_text_plain("Internal Server Error")
                    .send_to_client();
                return;
            }
        }
    };
    // Build lazily, once per sandbox. Reached only past the health, JA4, and
    // counters short-circuits, so none of those pays for construction.
    //
    // `initialize` builds only when the sandbox is empty, retains only
    // success, and returns the error unchanged. A failed build hands back its
    // error router as the error payload: that serves this request and is then
    // dropped, so a transient config-store failure cannot pin the sandbox into
    // permanent error mode, and the next callback retries construction.
    let failed_build = sandbox
        .initialize(|| {
            let _appbuild = timings.span(Phase::AppBuild);
            let (app, state) = TrustedServerApp::build_app_with_state(&runtime_stores);
            match state {
                Some(state) => Ok(RetainedApp { app, state }),
                None => Err(app),
            }
        })
        .err();

    let (app, app_state): (&edgezero_core::app::App, Option<Arc<AppState>>) =
        match (failed_build.as_ref(), sandbox.state()) {
            (Some(app), _) => (app, None),
            (None, Some(retained)) => (&retained.app, Some(Arc::clone(&retained.state))),
            (None, None) => {
                log::error!("no application available after initialization");
                FastlyResponse::from_status(fastly::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_body_text_plain("Internal Server Error")
                    .send_to_client();
                return;
            }
        };

    let settings_snapshot = app_state.as_ref().map(|state| Arc::clone(&state.settings));
    let counters =
        SandboxCounters::capture(sandbox, ordinal, request_id, settings_snapshot.as_deref());
    let server_timing_enabled = settings_snapshot
        .as_deref()
        .is_some_and(|settings| settings.observability.server_timing_enabled);
    let access_sample_rate = settings_snapshot
        .as_deref()
        .map_or(0.0, |settings| settings.tinybird.access_sample_rate);
    let access_telemetry_enabled = settings_snapshot
        .as_deref()
        .is_some_and(|settings| settings.tinybird.enabled && settings.tinybird.access_enabled);
    let publisher_domain = settings_snapshot
        .as_deref()
        .map_or("unknown", |settings| settings.publisher.domain.as_str());
    let trusted_client_ip = settings_snapshot
        .as_deref()
        .and_then(|settings| settings.trusted_client_ip.as_ref());

    // Preserve the public origin before native sanitation removes its source
    // fields. The frozen decision carries no authentication material.
    let forwarder = settings_snapshot
        .as_deref()
        .map(|settings| compat::capture_fastly_forwarder(&mut req, settings));

    // Resolve the trusted client IP, then strip client-spoofable forwarded
    // headers before dispatch. One call keeps resolution ahead of the
    // sanitization that removes the headers it reads.
    let resolved_client_ip = if trace_path {
        resolve_client_ip(&req, req.get_client_ip_addr(), trusted_client_ip)
    } else {
        compat::resolve_and_sanitize_client_ip(&mut req, trusted_client_ip)
    };

    // Retain the method before dispatch consumes the request, including trace paths.
    let request_method = req.get_method().clone();

    if !trace_path {
        // Re-inject a trusted TLS scheme signal after sanitization has stripped any
        // client-sent fastly-ssl header. Setting it from Fastly's native TLS
        // metadata here is authoritative. detect_request_scheme in http_util checks
        // this header so scheme-sensitive logic produces https URLs on HTTPS traffic.
        if req.get_tls_protocol().ok().flatten().is_some()
            || req.get_tls_cipher_openssl_name().ok().flatten().is_some()
        {
            req.set_header("fastly-ssl", "1");
        }

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
    }

    // Capture metadata from the original FastlyRequest before conversion. These
    // accessors only return real values on the client request, so store them in
    // request extensions for build_per_request_services and EC bot classification.
    let client_info = client_info_from_request(&req, resolved_client_ip);
    let client_ip = client_info.client_ip;
    let device_signals = (!trace_path).then(|| derive_device_signals(&req));

    // Dispatch directly through the EdgeZero router without an intermediate
    // fastly::Response conversion. That preserves duplicate header values such
    // as multiple Set-Cookie headers.
    let mut response = match into_core_request(req) {
        Ok(mut core_req) => {
            if let Some(forwarder) = forwarder {
                core_req.extensions_mut().insert(forwarder);
            }
            core_req.extensions_mut().insert(config_store);
            if let Some(device_signals) = device_signals {
                core_req.extensions_mut().insert(device_signals);
            }
            core_req.extensions_mut().insert(client_info);
            core_req.extensions_mut().insert(timings.handle().clone());
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

    if response
        .extensions()
        .get::<TraceTerminalResponse>()
        .is_some()
    {
        // Trace responses keep their exact hardened headers; no sandbox counters.
        send_response_to_client(response, None, &timings, None);
        return;
    }

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
            match load_settings_from_config_store(&runtime_stores) {
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
                    // The compiled-in version needs no settings, so a deploy that cannot
                    // load its config still reports which build is serving.
                    apply_git_version_header(&mut response);
                }
            }
        }
    }

    if let Some(policy) = asset_cache_policy {
        policy.apply_after_route_finalization(&mut response, EdgeCacheHeader::SurrogateControl);
    }

    let send_context = SendContext {
        timings: timings.clone(),
        server_timing_enabled,
        method: request_method.as_str(),
        publisher_domain,
        access_sample_rate,
        access_telemetry_enabled,
    };
    let access_http_client = platform::FastlyPlatformHttpClient::default();

    if let Some(mut ec_state) = ec_state {
        let limiter = FastlyRateLimiter::new(RATE_COUNTER_NAME);
        if let Some(state) = app_state.as_deref() {
            let graph = || {
                if state.identity_graph.is_none() {
                    log::warn!("identity graph unavailable: ec.ec_store is not configured");
                }
                state.identity_graph.clone()
            };
            run_post_send_steps(
                || {
                    let mut outcome = None;
                    finalize_send_and_flush(
                        PostRouteInput {
                            runtime: Some(&state.identity),
                            settings: &state.settings,
                            ec_state: &mut ec_state,
                            graph: &graph,
                            rate_limiter: &limiter,
                            timings: Some(&timings),
                        },
                        response,
                        &mut |response| {
                            outcome = Some(send_edgezero_response(
                                response,
                                request_filter_effects.as_ref(),
                                &send_context,
                                counters.as_ref(),
                            ));
                        },
                    );
                    outcome.expect("finalize_send_and_flush always sends the response")
                },
                |outcome| {
                    emit_access_telemetry_after_send(
                        &access_http_client,
                        &state.settings,
                        &outcome,
                        &timings,
                    )
                },
            );
            return;
        }
        match load_settings_from_config_store(&runtime_stores) {
            Ok(settings) => {
                // No application state was built, so the identity runtime
                // comes from the reloaded settings. Without one, finalize and
                // the withdrawal retry still run; only phase 4 is skipped.
                let identity = match build_identity_runtime_from_reloaded_settings(&settings) {
                    Ok(identity) => {
                        ec_state
                            .ec_context
                            .set_identity_runtime(Arc::clone(&identity));
                        Some(identity)
                    }
                    Err(e) => {
                        log::warn!(
                            "identity flush skipped: identity runtime unavailable after settings reload: {}",
                            e.current_context()
                        );
                        None
                    }
                };
                let graph = || {
                    require_identity_graph(&settings)
                        .map_err(|e| {
                            log::warn!("identity graph unavailable: {}", e.current_context());
                        })
                        .ok()
                };
                run_post_send_steps(
                    || {
                        let mut outcome = None;
                        finalize_send_and_flush(
                            PostRouteInput {
                                runtime: identity.as_deref(),
                                settings: &settings,
                                ec_state: &mut ec_state,
                                graph: &graph,
                                rate_limiter: &limiter,
                                timings: Some(&timings),
                            },
                            response,
                            &mut |response| {
                                outcome = Some(send_edgezero_response(
                                    response,
                                    request_filter_effects.as_ref(),
                                    &send_context,
                                    counters.as_ref(),
                                ));
                            },
                        );
                        outcome.expect("finalize_send_and_flush always sends the response")
                    },
                    |outcome| {
                        emit_access_telemetry_after_send(
                            &access_http_client,
                            &settings,
                            &outcome,
                            &timings,
                        )
                    },
                );
                return;
            }
            Err(e) => {
                log::warn!("EdgeZero EC finalize skipped: failed to reload settings: {e:?}");
            }
        }
    }

    let outcome = send_edgezero_response(
        response,
        request_filter_effects.as_ref(),
        &send_context,
        counters.as_ref(),
    );
    // The asset/admin/error fallback path: no `EcFinalizeState` (or the ec
    // finalize branch above failed), so there is no pull-sync dispatch here
    // at all — telemetry is the only post-send step. When `app_state` never
    // built there is nothing to emit either: `access_telemetry_enabled` was
    // necessarily false without a settings snapshot, so the outcome carries
    // no access snapshot, and reloading settings here could not change that.
    if let Some(settings) = settings_snapshot.as_deref() {
        emit_access_telemetry_after_send(&access_http_client, settings, &outcome, &timings);
    }
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
    if response
        .extensions()
        .get::<TraceTerminalResponse>()
        .is_some()
    {
        return;
    }
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
    // later in `send_edgezero_response` sees what was actually looked up;
    // 401 handling lives in the shared helper.
    middleware::write_back_geo_lookup_state(response, geo_info.as_ref());
}

/// What the post-route work reads. It takes the runtime and settings rather
/// than [`AppState`], because the settings-reload branch has none.
struct PostRouteInput<'a> {
    /// `None` when the runtime could not be built: finalize and the
    /// withdrawal retry still run, and phase 4 is skipped.
    runtime: Option<&'a IdentityRuntime>,
    settings: &'a Settings,
    ec_state: &'a mut EcFinalizeState,
    /// Builds the identity graph. Called only when finalize or the after-send
    /// work needs one, and at most once.
    graph: &'a dyn Fn() -> Option<KvIdentityGraph>,
    rate_limiter: &'a dyn RateLimiter,
    /// Only pre-send finalization contributes to request KV timing.
    timings: Option<&'a RequestTimings>,
}

/// Finalizes the response, hands it to `send`, then runs the after-send work:
/// the withdrawal retry first, then phase 4's one flush (spec 5.2), and logs
/// the request's `identity request measurement` line.
///
/// `send` is a callback so phase 4 runs only after the response is sent.
/// Returns phase 4's outcome, or `None` when finalize could not run.
fn finalize_send_and_flush(
    input: PostRouteInput<'_>,
    response: HttpResponse,
    send: &mut dyn FnMut(HttpResponse),
) -> Option<FlushOutcome> {
    let (outcome, measurement) = finalize_send_and_flush_measured(input, response, send);
    if let Some(measurement) = measurement {
        measurement.log();
    }
    outcome
}

/// As [`finalize_send_and_flush`], returning the measurement instead of
/// logging it. The measurement is `None` when finalize could not run.
fn finalize_send_and_flush_measured(
    input: PostRouteInput<'_>,
    mut response: HttpResponse,
    send: &mut dyn FnMut(HttpResponse),
) -> (Option<FlushOutcome>, Option<IdentityMeasurement>) {
    let PostRouteInput {
        runtime,
        settings,
        ec_state,
        graph,
        rate_limiter,
        timings,
    } = input;
    let partner_registry = match PartnerRegistry::from_config(&settings.ec.partners) {
        Ok(registry) => registry,
        Err(e) => {
            log::error!("EdgeZero EC finalize skipped: failed to build partner registry: {e:?}");
            send(response);
            return (None, None);
        }
    };
    // Every KV operation from here on counts into this; phase 1's read is
    // known from the snapshot the route left.
    let kv_counts = KvOperationCounts::default();
    let phase_one_read = !matches!(ec_state.ec_context.kv_snapshot(), EcKvSnapshot::NotRead);
    let mut built: Option<Option<KvIdentityGraph>> = None;
    let mut graph_once = || {
        built
            .get_or_insert_with(|| graph().map(|graph| graph.with_operation_counts(&kv_counts)))
            .clone()
    };

    let finalize_kv_graph = if ec_state.use_finalize_kv {
        graph_once().map(|graph| {
            if let Some(timings) = timings {
                graph.with_request_timings(timings)
            } else {
                graph
            }
        })
    } else {
        None
    };
    ec_finalize_response(
        settings,
        &mut ec_state.ec_context,
        finalize_kv_graph.as_ref(),
        &partner_registry,
        ec_state.eids_cookie.as_deref(),
        ec_state.sharedid_cookie.as_deref(),
        &mut response,
    );
    let status = response.status();
    send(response);

    // A retry skipped because the graph is unavailable leaves the ID failed, so
    // it counts as a failed run.
    let withdrawal_after_send = ec_state.ec_context.has_failed_withdrawals();
    if withdrawal_after_send {
        match graph_once() {
            Some(graph) => retry_failed_withdrawals(&mut ec_state.ec_context, &graph),
            None => log::warn!("withdrawal retry skipped: identity graph unavailable"),
        }
    }
    let withdrawal_after_send_failed = ec_state.ec_context.has_failed_withdrawals();

    // A capture removal write that failed before send gets one more attempt.
    if ec_state.ec_context.has_pending_removals() {
        match graph_once() {
            Some(graph) => retry_pending_removals(&mut ec_state.ec_context, &graph),
            None => {
                log::warn!("capture removal retry skipped: identity graph unavailable");
                lose_pending_removals(&mut ec_state.ec_context);
            }
        }
    }

    // Positive capture IDs from a request that is not the writer wait in the
    // pending key for the next writer.
    if ec_state.ec_context.has_capture_writes() {
        match graph_once() {
            Some(graph) => defer_capture_writes(&mut ec_state.ec_context, &graph),
            None => log::warn!("capture writes dropped: identity graph unavailable"),
        }
    }

    let outcome = flush_after_send(
        runtime,
        settings,
        ec_state,
        &mut graph_once,
        status,
        rate_limiter,
    );
    let measurement = ec_state.ec_context.identity_measurement(&MeasurementInput {
        phase_one_read,
        kv_reads: kv_counts.reads(),
        kv_writes: kv_counts.writes(),
        flush: Some(&outcome),
        withdrawal_after_send,
        withdrawal_after_send_failed,
    });
    (Some(outcome), Some(measurement))
}

/// Phase 4's one flush, or [`FlushOutcome::Skipped`] when it cannot run.
fn flush_after_send(
    runtime: Option<&IdentityRuntime>,
    settings: &Settings,
    ec_state: &mut EcFinalizeState,
    graph_once: &mut dyn FnMut() -> Option<KvIdentityGraph>,
    status: edgezero_core::http::StatusCode,
    rate_limiter: &dyn RateLimiter,
) -> FlushOutcome {
    let Some(runtime) = runtime else {
        return FlushOutcome::Skipped;
    };
    if !ec_state.is_real_browser || !may_flush(&ec_state.ec_context) {
        return FlushOutcome::Skipped;
    }
    let Some(graph) = graph_once() else {
        log::debug!("identity flush skipped: identity graph unavailable");
        return FlushOutcome::Skipped;
    };
    run_after_send(AfterSendInput {
        runtime,
        settings,
        services: &ec_state.services,
        graph: &graph,
        context: &mut ec_state.ec_context,
        status,
        rate_limiter,
    })
}

/// Completes the identity lifecycle, including send and persistence, before
/// access-telemetry emission.
///
/// Every `edgezero_main` site that has both steps routes through this
/// function, so the ordering is owned in exactly one place and the
/// sequence test can instrument it; `request_elapsed` is already stamped
/// before identity persistence because `send_edgezero_response` stamps it
/// before returning.
fn run_post_send_steps<T>(identity: impl FnOnce() -> T, emit_access_telemetry: impl FnOnce(T)) {
    let outcome = identity();
    emit_access_telemetry(outcome);
}

/// Builds and dispatches the access row after a response's transport attempt,
/// when access telemetry is enabled and this request is sampled in.
///
/// Called last at every `send_edgezero_response` call site in
/// [`edgezero_main`] — after `finalize_send_and_flush` on the two
/// EC-finalized paths, and directly after send on the asset/admin/error
/// fallback path, which never builds an [`EcFinalizeState`] or route-scoped
/// `RuntimeServices` at all. The Tinybird transport context is therefore
/// constructed fresh from `settings` here rather than threaded through
/// either of those per-route types, so every response class can emit.
///
/// Sampling happens before snapshot construction, using the same configured
/// rate stored in the row. Disabled and sampled-out requests have no snapshot
/// and return silently. Row construction, setup and request-start failures log
/// one warning. The host-owned ingest request continues after this callback;
/// later transport errors, HTTP status and remote acceptance are unobserved.
/// Credentials were resolved during settings load.
fn emit_access_telemetry_after_send(
    http_client: &dyn PlatformHttpClient,
    settings: &Settings,
    outcome: &DeliveryOutcome,
    timings: &RequestTimings,
) {
    if !settings.tinybird.enabled || !settings.tinybird.access_enabled {
        return;
    }

    // No snapshot means access telemetry was disabled or sampled out before
    // the response was sent; there is nothing to emit.
    let Some(snapshot) = &outcome.snapshot else {
        return;
    };

    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let epoch_ms = u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX);
    let row = access_event_row(snapshot, &timings.snapshot(), outcome.result, epoch_ms);
    let result = futures::executor::block_on(async {
        let target = tinybird::TinybirdEventsTarget::from_access_config(&settings.tinybird)?;
        tinybird::emit_access_event(http_client, &target, row).await
    });
    if let Err(error) = result {
        log::warn!("access telemetry emission dropped: {error:?}");
    }
}

/// Per-response context threaded into [`send_edgezero_response`] so the
/// function stays at or under seven parameters.
struct SendContext<'a> {
    /// The request's phase-timing collector.
    timings: RequestTimings,
    /// Whether `observability.server_timing_enabled` is set.
    server_timing_enabled: bool,
    /// The request's HTTP method, captured before the request was consumed
    /// by dispatch.
    method: &'a str,
    /// The configured publisher domain, borrowed from the settings snapshot.
    publisher_domain: &'a str,
    /// The configured access-telemetry sample rate.
    access_sample_rate: f64,
    /// Whether `tinybird.enabled` and `tinybird.access_enabled` were both
    /// set when settings were first read. Gates building the
    /// [`AccessTelemetrySnapshot`] at all: the snapshot costs env reads and
    /// `String` allocations on the pre-send path, which a disabled
    /// deployment (the default) should not pay.
    access_telemetry_enabled: bool,
}

/// Outcome of handing a finalized response to the client.
pub(crate) struct DeliveryOutcome {
    /// Outcome of the local body handoff, emitted as `delivery_result`.
    /// This does not establish confirmed client receipt.
    pub result: DeliveryResult,
    /// Access-telemetry dimensions captured for this response at the
    /// freeze point. `None` when access telemetry was disabled or sampled out;
    /// the emitter treats that as nothing to send.
    pub snapshot: Option<AccessTelemetrySnapshot>,
}

/// Thin Fastly-adapter wrapper around
/// [`append_server_timing_if_private`], the freeze point shared with the
/// Axum adapter's terminal timing layer. See that function's doc for the
/// emission rules (always stamps `mark_headers_ready`; appends rather than
/// overwrites; never promotes a response to shared-cacheable).
pub(crate) fn apply_server_timing_header(
    response: &mut HttpResponse,
    timings: &RequestTimings,
    server_timing_enabled: bool,
) {
    append_server_timing_if_private(response, timings, server_timing_enabled);
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
///
/// The `Ok(())` arm exists for the classifier's totality, not for the
/// production caller: `send_edgezero_response` consumes this value only in
/// its `Err` branch and re-derives the success outcome from
/// `streaming_body.finish()`.
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

/// Classifies the transport's finish result after a successful body drive.
fn classify_stream_finish(
    finish_result: std::io::Result<()>,
    counter_context: &str,
) -> DeliveryResult {
    match finish_result {
        Ok(()) => DeliveryResult::Complete,
        Err(error) => {
            log::error!("failed to finish EdgeZero streaming body{counter_context}: {error}");
            DeliveryResult::Partial
        }
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
    counters: Option<&SandboxCounters>,
) -> DeliveryOutcome {
    finalize_send_response_headers(
        &mut response,
        request_filter_effects,
        &context.timings,
        context.server_timing_enabled,
    );

    // Built right after the freeze point and before `into_parts()`
    // consumes `response`: nothing else survives to post-send on every
    // path (the request was consumed by dispatch, and `EcFinalizeState`
    // is absent on asset, admin, and error paths). Skipped entirely when
    // access telemetry is disabled, so the default configuration pays no
    // env reads or allocations here.
    let snapshot = sampled_access_snapshot(
        context,
        || rand::thread_rng().r#gen::<f64>(),
        || build_access_telemetry_snapshot(&response, context),
    );

    send_response_to_client(response, counters, &context.timings, snapshot)
}

/// Sends a response, attaching sandbox counters first when supplied.
fn send_response_to_client(
    mut response: HttpResponse,
    counters: Option<&SandboxCounters>,
    timings: &RequestTimings,
    snapshot: Option<AccessTelemetrySnapshot>,
) -> DeliveryOutcome {
    // Captured before the body is consumed so post-commitment failures can be
    // matched back to the response that carried these counters.
    let counter_context = counters.map_or_else(String::new, |counters| {
        format!(
            " [instance={} ordinal={} request={}]",
            instance_id(),
            counters.ordinal,
            counters.request_id
        )
    });

    // Before headers commit, including before `stream_to_client` below.
    if let Some(counters) = counters.as_ref() {
        attach_sandbox_counters(&mut response, counters);
    }

    let (parts, body) = response.into_parts();

    match body {
        EdgeBody::Stream(_) => {
            let skeleton = compat::to_fastly_response_skeleton(HttpResponse::from_parts(
                parts,
                EdgeBody::empty(),
            ));
            let (counting, drive_result) =
                drive_streaming_body(body, skeleton.stream_to_client(), timings);
            let bytes = counting.bytes();
            let streaming_body = counting.into_inner();
            // Computed before `drive_result` is matched by value below, since
            // the `Err` arm there moves its `Report` out.
            let result = classify_stream_delivery(&drive_result, bytes);
            match drive_result {
                Ok(()) => DeliveryOutcome {
                    result: classify_stream_finish(streaming_body.finish(), &counter_context),
                    snapshot,
                },
                Err(e) => {
                    // After commitment: log and stop. Returning an error here
                    // would let the SDK attempt a second response. Counters
                    // already went out with the headers, so the failure is
                    // tagged with the same identity for reconciliation.
                    log::error!("EdgeZero streaming failed{counter_context}: {e:?}");
                    drop(streaming_body);
                    DeliveryOutcome { result, snapshot }
                }
            }
        }
        once => {
            record_buffered_delivery(&once, timings);
            compat::to_fastly_response(HttpResponse::from_parts(parts, once)).send_to_client();
            DeliveryOutcome {
                result: DeliveryResult::Complete,
                snapshot,
            }
        }
    }
}

/// Samples once before doing snapshot work. Neither closure runs when disabled,
/// and sampled-out requests never build dimensions or read environment values.
fn sampled_access_snapshot(
    context: &SendContext<'_>,
    roll: impl FnOnce() -> f64,
    build: impl FnOnce() -> AccessTelemetrySnapshot,
) -> Option<AccessTelemetrySnapshot> {
    if !context.access_telemetry_enabled
        || !tinybird::sampled_in(context.access_sample_rate, roll())
    {
        return None;
    }
    Some(build())
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
        method: context.method.to_owned(),
        status: response.status().as_u16(),
        route_class,
        route_template,
        publisher_domain: context.publisher_domain.to_owned(),
        env: resolve_env_dimension(),
        service_id: env_var_or_unknown(ENV_FASTLY_SERVICE_ID),
        pop: env_var_or_unknown(ENV_FASTLY_POP),
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

/// Apply terminal response effects before evaluating Server-Timing at the send freeze point.
fn finalize_send_response_headers(
    response: &mut HttpResponse,
    request_filter_effects: Option<&RequestFilterEffects>,
    timings: &RequestTimings,
    server_timing_enabled: bool,
) {
    apply_terminal_response_effects(response, request_filter_effects);
    apply_server_timing_header(response, timings, server_timing_enabled);
}

/// Apply late response effects and restore cache-privacy invariants before headers commit.
fn apply_terminal_response_effects(
    response: &mut HttpResponse,
    request_filter_effects: Option<&RequestFilterEffects>,
) {
    if response
        .extensions()
        .get::<TraceTerminalResponse>()
        .is_some()
    {
        return;
    }
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

/// Builds the shared graph over the configured identity store.
pub(crate) fn maybe_identity_graph(settings: &Settings) -> Option<KvIdentityGraph> {
    require_identity_graph(settings).ok()
}

/// Constructs a `KvIdentityGraph` from settings, or returns an error if the
/// `ec_store` config is not set.
///
/// Deliberately untimed for retained application state. Request-path callers
/// wrap a clone with `with_request_timings`, or use
/// [`require_identity_graph_with_timing`] when rebuilding from settings.
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
/// Request-path sibling of [`require_identity_graph`].
pub(crate) fn require_identity_graph_with_timing(
    settings: &Settings,
    timings: &RequestTimings,
) -> Result<KvIdentityGraph, Report<TrustedServerError>> {
    require_identity_graph(settings).map(|graph| graph.with_request_timings(timings))
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
    use std::sync::Mutex;

    use super::*;
    use crate::app::TrustedServerApp;
    use base64::Engine as _;
    use edgezero_core::body::Body as EdgeBody;
    use edgezero_core::http::response_builder;
    use edgezero_core::http::{HeaderValue, Method, StatusCode};
    use edgezero_core::ingress::{
        CapturedTarget, HeaderFidelity, InboundOrigin, OriginSource, RequestIngress,
        TargetUnavailable,
    };
    use fastly::mime;
    use std::time::Duration;
    use trusted_server_core::ec::identity::measurement::RouteKind;
    use trusted_server_core::integrations::HeaderMutation;
    use trusted_server_core::platform::{
        PlatformError, PlatformHttpRequest, PlatformPendingRequest, PlatformResponse,
        PlatformSelectResult, RuntimeServices,
    };
    use trusted_server_core::request_timing::AuctionWaitPlacement;

    #[derive(Default)]
    struct RecordingAccessHttpClient {
        requests: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformHttpClient for RecordingAccessHttpClient {
        async fn send(
            &self,
            request: PlatformHttpRequest,
        ) -> Result<PlatformResponse, Report<PlatformError>> {
            self.requests
                .lock()
                .expect("should lock recorded requests")
                .push(request.request.uri().to_string());
            Err(Report::new(PlatformError::Unsupported))
        }

        async fn send_async(
            &self,
            request: PlatformHttpRequest,
        ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
            self.requests
                .lock()
                .expect("should lock recorded requests")
                .push(request.request.uri().to_string());
            Err(Report::new(PlatformError::Unsupported))
        }

        async fn select(
            &self,
            _pending_requests: Vec<PlatformPendingRequest>,
        ) -> Result<PlatformSelectResult, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    const TEST_SETTINGS_TOML: &str = r#"
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
            "#;

    fn test_settings() -> Settings {
        Settings::from_toml(TEST_SETTINGS_TOML).expect("should parse test settings")
    }

    const EC_ID: &str = "0000000000000000000000000000000000000000000000000000000000000000.ABC123";

    /// Allows every call.
    struct AllowAll;

    impl RateLimiter for AllowAll {
        fn exceeded(&self, _key: &str, _limit: u32) -> Result<bool, Report<TrustedServerError>> {
            Ok(false)
        }

        fn record(&self, _key: &str) -> Result<(), Report<TrustedServerError>> {
            Ok(())
        }

        fn count(&self, _key: &str) -> Result<u32, Report<TrustedServerError>> {
            Ok(0)
        }
    }

    /// Marks `request` as sent by a real browser at a client address.
    fn as_browser(request: &mut HttpRequest) {
        request.extensions_mut().insert(DeviceSignals::derive(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
            Some("t13d1516h2_8daaf6152771_b186095e22b6"),
            Some("1:65536;2:0;4:6291456;6:262144"),
        ));
        request
            .extensions_mut()
            .insert(trusted_server_core::platform::ClientInfo {
                client_ip: Some("192.0.2.10".parse().expect("should parse the client IP")),
                ..trusted_server_core::platform::ClientInfo::default()
            });
    }

    /// A browser document navigation carrying `cookie`.
    fn navigation(cookie: Option<&str>) -> HttpRequest {
        let mut builder = edgezero_core::http::request_builder()
            .method(edgezero_core::http::Method::GET)
            .uri("https://test-publisher.com/some-page")
            .header("sec-fetch-dest", "document");
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        let mut request = builder
            .body(EdgeBody::empty())
            .expect("should build request");
        as_browser(&mut request);
        request
    }

    /// Polls `future` to completion on this thread without entering an
    /// executor, so code under test that blocks on its own futures, such as
    /// the hold dispatch of an auction navigation, can run inside it. The
    /// scripted network is ready at once, so a future that stays pending is a
    /// test bug.
    fn poll_ready<F: core::future::Future>(future: F) -> F::Output {
        let waker = futures::task::noop_waker();
        let mut context = core::task::Context::from_waker(&waker);
        let mut future = core::pin::pin!(future);
        for _ in 0..10_000 {
            if let core::task::Poll::Ready(output) = future.as_mut().poll(&mut context) {
                return output;
            }
        }
        unreachable!("the future should complete against the scripted network")
    }

    /// Routes `request`, then finalizes, sends and flushes it as
    /// `edgezero_main` does, keeping the sent response.
    fn drive_request_for_test(
        state: &Arc<AppState>,
        request: HttpRequest,
    ) -> (HttpResponse, Option<FlushOutcome>) {
        drive_with_runtime(state, request, Some(&state.identity), |_| {})
    }

    /// As [`drive_request_for_test`], with the runtime the post-route work
    /// gets: `None` is the settings-reload branch without a runtime.
    fn drive_with_runtime(
        state: &Arc<AppState>,
        request: HttpRequest,
        runtime: Option<&IdentityRuntime>,
        prepare: impl FnOnce(&mut EcFinalizeState),
    ) -> (HttpResponse, Option<FlushOutcome>) {
        let router = TrustedServerApp::routes_for_state(state);
        let mut response = poll_ready(router.oneshot(request)).expect("should route the request");
        let Some(mut ec_state) = response.extensions_mut().remove::<EcFinalizeState>() else {
            return (response, None);
        };
        prepare(&mut ec_state);
        let mut sent = None;
        let graph = || state.identity_graph.clone();
        let outcome = finalize_send_and_flush(
            PostRouteInput {
                runtime,
                settings: &state.settings,
                ec_state: &mut ec_state,
                graph: &graph,
                rate_limiter: &AllowAll,
                timings: None,
            },
            response,
            &mut |response| sent = Some(response),
        );
        (sent.expect("should send the response"), outcome)
    }

    /// As [`drive_request_for_test`], keeping the request's measurement.
    fn drive_measured_for_test(
        state: &Arc<AppState>,
        request: HttpRequest,
    ) -> (Option<FlushOutcome>, IdentityMeasurement) {
        let router = TrustedServerApp::routes_for_state(state);
        let mut response =
            futures::executor::block_on(router.oneshot(request)).expect("should route the request");
        let mut ec_state = response
            .extensions_mut()
            .remove::<EcFinalizeState>()
            .expect("should leave EC state on the response");
        let graph = || state.identity_graph.clone();
        let (outcome, measurement) = finalize_send_and_flush_measured(
            PostRouteInput {
                runtime: Some(&state.identity),
                settings: &state.settings,
                ec_state: &mut ec_state,
                graph: &graph,
                rate_limiter: &AllowAll,
                timings: None,
            },
            response,
            &mut |_| {},
        );
        (
            outcome,
            measurement.expect("should measure a finalized request"),
        )
    }

    /// A returning visitor's live row, without a cluster size so the flush
    /// has one change to write.
    fn seeded_state() -> (
        Arc<AppState>,
        Arc<trusted_server_core::ec::identity::test_support::WriteCountingEcKv>,
    ) {
        let (graph, store) = KvIdentityGraph::write_counting("drive-store");
        let consent = trusted_server_core::ec::identity::test_support::allowing_consent();
        let entry = trusted_server_core::ec::kv_types::KvEntry::new(
            &consent,
            None,
            1_000,
            "test-publisher.com",
        );
        graph.create(EC_ID, &entry).expect("should seed the row");
        store.reset_counts();
        let state = crate::app::app_state_with_graph_for_test(test_settings(), graph);
        (state, store)
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
            country: "unknown".to_owned(),
            template_cache_state: "unknown".to_owned(),
            body_mode: "buffered",
            sample_rate: 0.0,
        }
    }

    #[test]
    fn trace_dispatch_terminal_asset_bypasses_hostile_native_finalizers() {
        let mut settings = test_settings();
        settings
            .integrations
            .insert_config(
                "gpt_diagnostics",
                &serde_json::json!({"enabled":true,"trace_page_enabled":true}),
            )
            .expect("should configure trace");
        settings.handlers.insert(0,serde_json::from_value(serde_json::json!({"path":"^/_ts/trace","username":"example-user","password":"example-password"}))
            .expect("should configure trace protection"));
        let mut request = edgezero_core::http::request_builder()
            .uri("https://publisher.example.com/_ts/trace/assets/v1.js")
            .header(
                "authorization",
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD
                        .encode("example-user:example-password")
                ),
            )
            .body(EdgeBody::empty())
            .expect("should build protected asset request");
        let trusted_server_core::trace::TracePreflight::Ready(dispatch) =
            trusted_server_core::trace::preflight(&settings, &mut request)
        else {
            panic!("should accept protected fixed asset");
        };
        let mut response = dispatch.fixed_asset(b"example immutable asset");
        let original = response.headers().clone();
        let effects = RequestFilterEffects {
            request_headers: Vec::new(),
            response_headers: vec![
                HeaderMutation::set("content-security-policy", "default-src *"),
                HeaderMutation::set("content-type", "text/plain"),
                HeaderMutation::append("set-cookie", "example=operator"),
                HeaderMutation::set("cache-control", "public, max-age=3600"),
            ],
        };
        apply_entry_point_finalize_headers(
            &settings,
            &mut response,
            None,
            &GeoLookupState::NotAttempted,
            &RequestTimings::new(),
        );
        apply_terminal_response_effects(&mut response, Some(&effects));
        assert_eq!(
            response.headers(),
            &original,
            "should bypass every ordinary trace finalizer and preserve protected digest"
        );
        let native = compat::to_fastly_response(response);
        assert_eq!(
            native.get_header("etag").map(HeaderValue::as_bytes),
            original.get("etag").map(HeaderValue::as_bytes),
            "should preserve strong ETag through native conversion"
        );
        assert!(
            native.get_header("set-cookie").is_none(),
            "should never acquire an operator cookie"
        );
    }

    #[test]
    fn trace_dispatch_terminal_actions_errors_and_head_preserve_native_contract() {
        let mut settings = test_settings();
        settings
            .integrations
            .insert_config(
                "gpt_diagnostics",
                &serde_json::json!({"enabled":true,"trace_page_enabled":true}),
            )
            .expect("should configure trace");
        settings.handlers.insert(0,serde_json::from_value(serde_json::json!({"path":"^/_ts/trace","username":"example-user","password":"example-password"}))
            .expect("should configure trace protection"));
        let hook = trusted_server_core::trace::TracePreDispatchHook::new(
            Arc::new(settings.clone()),
            Arc::new(|_| panic!("should never query metadata for actions, errors or HEAD")),
        );
        let effects = RequestFilterEffects {
            request_headers: Vec::new(),
            response_headers: vec![
                HeaderMutation::set("content-security-policy", "default-src *"),
                HeaderMutation::set("content-type", "text/plain"),
                HeaderMutation::append("set-cookie", "example=operator"),
                HeaderMutation::set("cache-control", "public, max-age=3600"),
            ],
        };
        for (method, path, authenticated, expected, action) in [
            (
                Method::GET,
                "/_ts/trace",
                false,
                StatusCode::UNAUTHORIZED,
                None,
            ),
            (
                Method::HEAD,
                "/_ts/trace",
                false,
                StatusCode::UNAUTHORIZED,
                None,
            ),
            (Method::HEAD, "/_ts/trace", true, StatusCode::OK, None),
            (
                Method::PATCH,
                "/_ts/trace/state",
                true,
                StatusCode::METHOD_NOT_ALLOWED,
                None,
            ),
            (
                Method::POST,
                "/_ts/trace/enable",
                true,
                StatusCode::OK,
                Some("enable"),
            ),
            (
                Method::POST,
                "/_ts/trace/end",
                true,
                StatusCode::OK,
                Some("end"),
            ),
        ] {
            let head = method == Method::HEAD;
            let mut request = edgezero_core::http::request_builder()
                .method(method)
                .uri(path)
                .header("host", "publisher.example.com")
                .body(EdgeBody::empty())
                .expect("should build terminal trace request");
            if authenticated {
                request.headers_mut().insert(
                    "authorization",
                    HeaderValue::from_str(&format!(
                        "Basic {}",
                        base64::engine::general_purpose::STANDARD
                            .encode("example-user:example-password")
                    ))
                    .expect("should encode fictional trace credentials"),
                );
            }
            if let Some(action) = action {
                request.headers_mut().insert(
                    "origin",
                    HeaderValue::from_static("https://publisher.example.com"),
                );
                request
                    .headers_mut()
                    .insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
                request.headers_mut().insert(
                    "x-ts-trace-action",
                    HeaderValue::from_str(action).expect("should encode fixed action"),
                );
                request.extensions_mut().insert(RequestIngress::new(
                    CapturedTarget::Unavailable(TargetUnavailable::NotExposed),
                    Some(
                        InboundOrigin::parse(
                            "https",
                            "publisher.example.com",
                            OriginSource::RuntimeUri,
                        )
                        .expect("should validate fictional runtime origin"),
                    ),
                    HeaderFidelity::default(),
                ));
            }
            let mut response = futures::executor::block_on(
                edgezero_core::router::PreDispatchHook::handle(&hook, &mut request),
            )
            .expect("should build local trace response")
            .expect("should intercept reserved request");
            assert_eq!(
                response.status(),
                expected,
                "should preserve action and preflight status"
            );
            let original = response.headers().clone();
            apply_entry_point_finalize_headers(
                &settings,
                &mut response,
                None,
                &GeoLookupState::NotAttempted,
                &RequestTimings::new(),
            );
            apply_terminal_response_effects(&mut response, Some(&effects));
            assert_eq!(
                response.headers(),
                &original,
                "should bypass hostile ordinary finalizers for every terminal response"
            );
            let mut native = compat::to_fastly_response(response);
            for (name, value) in &original {
                assert_eq!(
                    native.get_header(name.as_str()).map(HeaderValue::as_bytes),
                    Some(value.as_bytes()),
                    "should preserve complete trace headers through native conversion"
                );
            }
            assert_eq!(
                native
                    .get_headers()
                    .filter(|(name, _)| name.as_str() == "set-cookie")
                    .count(),
                usize::from(action.is_some()),
                "should emit only one deliberate action cookie and none on reads or errors"
            );
            if head {
                assert!(
                    native.take_body_bytes().is_empty(),
                    "should preserve bodyless trace HEAD through native conversion"
                );
            }
        }
    }

    #[test]
    fn identity_measurement_records_counts_for_a_writer_navigation() {
        let (state, _store) = seeded_state();
        let cookie = format!("ts-ec={EC_ID}");

        let (outcome, measurement) = drive_measured_for_test(&state, navigation(Some(&cookie)));

        assert_eq!(outcome, Some(FlushOutcome::Written), "should flush once");
        assert_eq!(
            measurement.cookie_header_bytes(),
            cookie.len(),
            "should hold the inbound Cookie header size"
        );
        assert_eq!(
            measurement.kv_reads(),
            2,
            "should count the phase 1 read and the pending-key read"
        );
        assert_eq!(measurement.kv_writes(), 1, "should count the one write");
        assert_eq!(
            measurement.route(),
            RouteKind::Writer,
            "should label a returning navigation as the writer"
        );
        assert_eq!(
            measurement.flush(),
            Some(FlushOutcome::Written),
            "should hold the phase 4 outcome"
        );
        assert_eq!(measurement.removal_lost(), 0, "should lose no removal");
        assert!(
            !measurement.withdrawal_after_send() && !measurement.withdrawal_after_send_failed(),
            "should need no after-send withdrawal run"
        );
    }

    #[test]
    fn identity_measurement_holds_no_identifier() {
        let (state, _store) = seeded_state();
        let cookie = format!("ts-ec={EC_ID}; ts-eids=secret-uid-value");

        let (_, measurement) = drive_measured_for_test(&state, navigation(Some(&cookie)));
        let line = measurement.to_string();

        assert!(
            line.contains(&format!("cookie_header_bytes={}", cookie.len())),
            "should still carry the cookie size: {line}"
        );
        assert!(!line.contains(EC_ID), "should not hold the EC ID: {line}");
        assert!(
            !line.contains("secret-uid-value"),
            "should not hold a cookie value: {line}"
        );
    }

    #[test]
    fn drive_request_for_test_counts_routing_finalize_and_flush_against_one_store() {
        let (state, store) = seeded_state();

        let (_, outcome) =
            drive_request_for_test(&state, navigation(Some(&format!("ts-ec={EC_ID}"))));

        assert_eq!(outcome, Some(FlushOutcome::Written), "should flush once");
        let writes = store.writes();
        assert_eq!(writes.lookups, 1, "should read the row once, in phase 1");
        assert_eq!(
            writes.if_generation_match, 1,
            "should write once with a CAS"
        );
        assert_eq!(writes.total(), 1, "should make no other write");
    }

    #[test]
    fn finalize_send_and_flush_sends_before_it_flushes() {
        let (state, store) = seeded_state();
        let router = TrustedServerApp::routes_for_state(&state);
        let mut response = futures::executor::block_on(
            router.oneshot(navigation(Some(&format!("ts-ec={EC_ID}")))),
        )
        .expect("should route the request");
        let mut ec_state = response
            .extensions_mut()
            .remove::<EcFinalizeState>()
            .expect("should carry EC state");
        let writes_at_send = std::cell::Cell::new(None);
        let timings = RequestTimings::new();
        let graph = || state.identity_graph.clone();

        let outcome = finalize_send_and_flush(
            PostRouteInput {
                runtime: Some(&state.identity),
                settings: &state.settings,
                ec_state: &mut ec_state,
                graph: &graph,
                rate_limiter: &AllowAll,
                timings: Some(&timings),
            },
            response,
            &mut |_| writes_at_send.set(Some(store.writes().total())),
        );

        assert_eq!(
            writes_at_send.get(),
            Some(0),
            "should send before any write"
        );
        assert_eq!(outcome, Some(FlushOutcome::Written), "should then flush");
        assert_eq!(store.writes().total(), 1, "should write once");
        assert!(
            timings.snapshot().kv_ms.is_none(),
            "the after-send flush must not add request-path KV timing"
        );
    }

    #[test]
    fn after_send_noop_states_skip_post_send_graph_factory() {
        let (state, _) = seeded_state();
        let router = TrustedServerApp::routes_for_state(&state);
        // A named route is never the writer, so phase 4 has nothing to do.
        let request = edgezero_core::http::request_builder()
            .method(edgezero_core::http::Method::GET)
            .uri("https://test-publisher.com/.well-known/trusted-server.json")
            .body(EdgeBody::empty())
            .expect("should build request");
        let mut response =
            futures::executor::block_on(router.oneshot(request)).expect("should route");
        let mut ec_state = response
            .extensions_mut()
            .remove::<EcFinalizeState>()
            .expect("should carry EC state");
        ec_state.use_finalize_kv = false;
        let calls = std::cell::Cell::new(0);
        let graph = || {
            calls.set(calls.get() + 1);
            None
        };

        let outcome = finalize_send_and_flush(
            PostRouteInput {
                runtime: Some(&state.identity),
                settings: &state.settings,
                ec_state: &mut ec_state,
                graph: &graph,
                rate_limiter: &AllowAll,
                timings: None,
            },
            response,
            &mut drop,
        );

        assert_eq!(outcome, Some(FlushOutcome::Skipped), "should skip phase 4");
        assert_eq!(calls.get(), 0, "should not build the graph");
    }

    #[test]
    fn no_response_sets_or_reads_the_pull_complete_cookie() {
        let (state, store) = seeded_state();
        let cookie = format!("ts-ec={EC_ID}; ts-ec-pull-complete=v1.example");

        let (response, _) = drive_request_for_test(&state, navigation(Some(&cookie)));

        assert!(
            response
                .headers()
                .get_all("set-cookie")
                .iter()
                .filter_map(|value| value.to_str().ok())
                .all(|value| !value.starts_with("ts-ec-pull-complete")),
            "should set no pull-complete cookie"
        );
        assert_eq!(
            store.writes().lookups,
            1,
            "should read the row whatever the old marker cookie says"
        );

        let (withdrawn, _) = drive_with_runtime(
            &state,
            navigation(Some(&cookie)),
            Some(&state.identity),
            withdraw,
        );
        let cookies = set_cookies(&withdrawn);
        assert!(
            cookies
                .iter()
                .any(|value| value.starts_with("ts-ec=;") && value.contains("Max-Age=0")),
            "control: finalize should still expire ts-ec on a withdrawal: {cookies:?}"
        );
        assert!(
            cookies
                .iter()
                .all(|value| !value.starts_with("ts-ec-pull-complete")),
            "should not expire a pull-complete cookie either: {cookies:?}"
        );
    }

    /// Turns the request's consent into an explicit withdrawal (GPC in a US
    /// state). Viceroy has no geo, so routing alone cannot produce one.
    fn withdraw(ec_state: &mut EcFinalizeState) {
        let consent = ec_state.ec_context.consent_mut();
        consent.jurisdiction =
            trusted_server_core::consent::jurisdiction::Jurisdiction::UsState("CA".to_owned());
        consent.gpc = true;
    }

    fn set_cookies(response: &HttpResponse) -> Vec<String> {
        response
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|value| value.to_str().ok().map(str::to_owned))
            .collect()
    }

    #[test]
    fn reload_without_identity_runtime_still_finalizes() {
        let (state, _) = seeded_state();

        let (response, outcome) = drive_with_runtime(
            &state,
            navigation(Some(&format!("ts-ec={EC_ID}"))),
            None,
            withdraw,
        );

        let cookies = set_cookies(&response);
        assert!(
            cookies
                .iter()
                .any(|value| value.starts_with("ts-ec=;") && value.contains("Max-Age=0")),
            "should still finalize and expire ts-ec without a runtime: {cookies:?}"
        );
        let (row, _) = state
            .identity_graph
            .as_ref()
            .expect("should hold the graph")
            .get(EC_ID)
            .expect("should read the row")
            .expect("should keep the tombstone");
        assert!(!row.consent.ok, "should still tombstone the row");
        assert_eq!(outcome, Some(FlushOutcome::Skipped), "should skip phase 4");
    }

    #[test]
    fn trace_path_classification_survives_core_request_conversion() {
        // `main` skips client-IP sanitization when the native path is a trace
        // path; the router hook later classifies the converted URI. Both must
        // agree, or unsanitized forwarded headers could reach ordinary dispatch.
        for url in [
            "https://example.com/_ts/trace",
            "https://example.com/_ts/trace/",
            "https://example.com/_ts/trace/enable?source=example",
            "https://example.com/_ts//trace",
            "https://example.com/_ts/./trace",
            "https://example.com/_ts/x/../trace",
            "https://example.com/_ts/%2e%2e/trace",
            "https://example.com/_ts%2Ftrace",
            "https://example.com/_ts/trace%2Fenable",
            "https://example.com/_TS/trace",
            "https://example.com/_tsx",
            "https://example.com/_ts-trace",
            "https://example.com/_ts/admin/keys",
            "https://example.com/auction",
            "https://example.com/",
        ] {
            let req = FastlyRequest::get(url);
            let native = is_trace_path(req.get_path());
            let core = into_core_request(req).expect("should convert the native request");

            assert_eq!(
                is_trace_path(core.uri().path()),
                native,
                "should classify {url} identically before and after conversion"
            );
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
    fn health_short_circuit_is_get_only_and_ignores_query() {
        for path in ["/health", "/health?probe=1"] {
            let url = format!("https://example.com{path}");
            assert!(
                health_response(&FastlyRequest::get(&url)).is_some(),
                "should bypass GET health"
            );
            assert!(
                health_response(&FastlyRequest::post(&url)).is_none(),
                "should time non-GET health normally"
            );
        }
    }

    #[test]
    fn health_response_reports_git_version_only() {
        let req = FastlyRequest::get("https://example.com/health");

        let response = health_response(&req).expect("should build health response");

        assert_eq!(
            response.get_header_str("x-ts-version"),
            trusted_server_core::constants::TS_GIT_VERSION,
            "should report the compiled-in git version on /health"
        );
        assert!(
            response.get_header("x-ts-fastly-version").is_none(),
            "should keep x-ts-fastly-version off the /health fast path"
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
    fn sandbox_counters_never_appear_on_cacheable_responses() {
        let counters = SandboxCounters {
            ordinal: 2,
            builds: 1,
            request_id: "request-example".to_owned(),
        };
        for policy in [
            None,
            Some("public, s-maxage=3600"),
            Some("private, max-age=60"),
            Some("private=\"set-cookie\""),
            Some("public, extension=\"private, no-store\""),
        ] {
            let mut response = HttpResponse::new(EdgeBody::empty());
            if let Some(policy) = policy {
                response.headers_mut().insert(
                    "cache-control",
                    HeaderValue::from_str(policy).expect("should encode cache policy"),
                );
            }
            apply_terminal_response_effects(&mut response, None);
            let original_headers = response.headers().clone();

            attach_sandbox_counters(&mut response, &counters);

            assert_eq!(
                response.headers(),
                &original_headers,
                "should preserve cache policy and omit all counters for {policy:?}"
            );
        }
    }

    #[test]
    fn sandbox_counters_follow_final_private_no_store_policy() {
        let counters = SandboxCounters {
            ordinal: 2,
            builds: 1,
            request_id: "request-example".to_owned(),
        };
        let mut response = response_builder()
            .header("cache-control", "public, s-maxage=3600")
            .body(EdgeBody::empty())
            .expect("should build response");
        response.extensions_mut().insert(TerminalPrivateResponse);
        apply_terminal_response_effects(&mut response, None);

        attach_sandbox_counters(&mut response, &counters);

        assert_eq!(
            response.headers()[sandbox::HEADER_SANDBOX_REQUEST_ID],
            "request-example",
            "should identify this uncached request"
        );
        assert_eq!(
            response.headers()[sandbox::HEADER_SANDBOX_ORDINAL],
            "2",
            "should report the current ordinal"
        );
        assert_eq!(
            response.headers()["cache-control"],
            "no-store, private",
            "should retain terminal privacy"
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
    fn send_freeze_applies_filter_effects_before_server_timing_privacy_gate() {
        let mut response = response_builder()
            .header("cache-control", "private, no-store")
            .body(EdgeBody::empty())
            .expect("should build response");
        let effects = RequestFilterEffects {
            request_headers: Vec::new(),
            response_headers: vec![HeaderMutation::set(
                "cache-control",
                "public, s-maxage=3600",
            )],
        };
        let context = send_context_fixture();
        drop(context.timings.span(Phase::Origin));

        finalize_send_response_headers(&mut response, Some(&effects), &context.timings, true);

        assert_eq!(response.headers()["cache-control"], "public, s-maxage=3600");
        assert!(
            response.headers().get("server-timing").is_none(),
            "late public cache policy must be visible to the Server-Timing privacy gate"
        );
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
            .http_client(Arc::new(
                crate::platform::FastlyPlatformHttpClient::default(),
            ))
            .geo(Arc::new(crate::platform::FastlyPlatformGeo))
            .client_info(trusted_server_core::platform::ClientInfo::default())
            .build()
    }

    #[test]
    fn ec_finalize_kv_lands_before_freeze() {
        // A pre-seeded EC entry (see fastly.toml's ec_identity_store fixture)
        // for a returning user carrying an eids cookie. Finalize confirms a
        // stale pre-route miss before headers freeze. Ordinary browser ingest
        // now queues for the after-send flush instead of writing here.
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
        let mut ec_context = trusted_server_core::ec::EcContext::read_from_request_with_geo(
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
        ec_context.set_eid_sync_source(trusted_server_core::ec::EidSyncSource::Navigation);
        ec_context.set_recovery_eligible(true);
        ec_context.set_kv_snapshot(EcKvSnapshot::Missing {
            ec_id: ec_id.to_owned(),
        });

        let mut ec_state = EcFinalizeState {
            ec_context,
            use_finalize_kv: true,
            eids_cookie: Some(eids_cookie),
            sharedid_cookie: None,
            is_real_browser: true,
            services,
        };
        let response = response_builder()
            .header("cache-control", "private, no-store")
            .body(EdgeBody::empty())
            .expect("should build EC finalize response fixture");
        let timings = RequestTimings::new();

        // Exercise the production finalize/send/flush boundary without a
        // live client connection. Header timing freezes in the send callback.
        let graph = || require_identity_graph(&settings).ok();
        let mut sent = None;
        finalize_send_and_flush(
            PostRouteInput {
                runtime: None,
                settings: &settings,
                ec_state: &mut ec_state,
                graph: &graph,
                rate_limiter: &AllowAll,
                timings: Some(&timings),
            },
            response,
            &mut |mut response| {
                apply_server_timing_header(&mut response, &timings, true);
                sent = Some(response);
            },
        );
        let response = sent.expect("should send the finalized response");
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
    fn streamed_delivery_row_reports_bytes_and_request_elapsed() {
        let timings = RequestTimings::new();
        let body = EdgeBody::stream(futures::stream::iter(vec![
            bytes::Bytes::from_static(b"hello "),
            bytes::Bytes::from_static(b"world"),
        ]));

        let (counting, drive_result) = drive_streaming_body(body, Vec::new(), &timings);
        drive_result.expect("streaming a well-formed body should not fail");
        let bytes = counting.bytes();
        let outcome = DeliveryOutcome {
            result: DeliveryResult::Complete,
            snapshot: Some(AccessTelemetrySnapshot {
                body_mode: "streamed",
                ..sample_access_snapshot()
            }),
        };

        assert_eq!(
            counting.into_inner(),
            b"hello world",
            "should write every byte to the underlying transport"
        );
        assert_eq!(
            bytes,
            "hello world".len() as u64,
            "should count bytes accepted by the underlying transport"
        );

        let snapshot = timings.snapshot();
        let row: serde_json::Value = serde_json::from_str(&access_event_row(
            outcome
                .snapshot
                .as_ref()
                .expect("should capture access dimensions"),
            &snapshot,
            outcome.result,
            0,
        ))
        .expect("should parse the emitted delivery row");
        assert_eq!(row["delivery_result"], "complete");
        assert_eq!(row["resp_bytes"], bytes);
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

    fn send_context_fixture() -> SendContext<'static> {
        SendContext {
            timings: RequestTimings::new(),
            server_timing_enabled: false,
            method: "GET",
            publisher_domain: "publisher.example.com",
            access_sample_rate: 0.25,
            access_telemetry_enabled: true,
        }
    }

    #[test]
    fn disabled_access_telemetry_skips_entropy_and_snapshot_work() {
        let mut context = send_context_fixture();
        context.access_telemetry_enabled = false;

        let snapshot = sampled_access_snapshot(
            &context,
            || panic!("disabled telemetry should not draw entropy"),
            || panic!("disabled telemetry should not construct dimensions"),
        );

        assert!(snapshot.is_none(), "should not build a disabled snapshot");
    }

    #[test]
    fn sampled_out_access_telemetry_skips_snapshot_and_transport() {
        let context = send_context_fixture();
        let snapshot = sampled_access_snapshot(
            &context,
            || context.access_sample_rate,
            || panic!("sampled-out telemetry should not construct dimensions"),
        );
        assert!(snapshot.is_none(), "should exclude the sampling boundary");
        let mut settings = test_settings();
        settings.tinybird.api_host = "ingest.example.com".to_owned();
        settings.tinybird.enabled = true;
        settings.tinybird.access_enabled = true;
        settings.tinybird.access_token_secret = Some(trusted_server_core::redacted::Redacted::new(
            "access-append-token".to_owned(),
        ));
        let http_client = RecordingAccessHttpClient::default();
        emit_access_telemetry_after_send(
            &http_client,
            &settings,
            &DeliveryOutcome {
                result: DeliveryResult::Complete,
                snapshot,
            },
            &context.timings,
        );

        assert!(
            http_client
                .requests
                .lock()
                .expect("should lock recorded requests")
                .is_empty(),
            "should not dispatch access telemetry for a sampled-out response"
        );
    }

    #[test]
    fn access_telemetry_setup_error_skips_transport_without_panicking() {
        let mut settings = test_settings();
        settings.tinybird.api_host = "ingest.example.com".to_owned();
        settings.tinybird.enabled = true;
        settings.tinybird.access_enabled = true;
        settings.tinybird.access_token_secret = None;
        let http_client = RecordingAccessHttpClient::default();

        emit_access_telemetry_after_send(
            &http_client,
            &settings,
            &DeliveryOutcome {
                result: DeliveryResult::Complete,
                snapshot: Some(sample_access_snapshot()),
            },
            &RequestTimings::new(),
        );

        assert!(
            http_client
                .requests
                .lock()
                .expect("should lock recorded requests")
                .is_empty(),
            "should not dispatch access telemetry without a resolved token"
        );
    }

    #[test]
    fn access_telemetry_dispatch_is_gated_and_best_effort() {
        for (enabled, access_enabled) in [(false, true), (true, false), (true, true)] {
            let mut settings = test_settings();
            settings.tinybird.api_host = "ingest.example.com".to_owned();
            settings.tinybird.enabled = enabled;
            settings.tinybird.access_enabled = access_enabled;
            settings.tinybird.access_token_secret = Some(
                trusted_server_core::redacted::Redacted::new("access-append-token".to_owned()),
            );
            let http_client = RecordingAccessHttpClient::default();

            emit_access_telemetry_after_send(
                &http_client,
                &settings,
                &DeliveryOutcome {
                    result: DeliveryResult::Complete,
                    snapshot: Some(sample_access_snapshot()),
                },
                &RequestTimings::new(),
            );

            assert_eq!(
                http_client
                    .requests
                    .lock()
                    .expect("should lock recorded requests")
                    .len(),
                usize::from(enabled && access_enabled),
                "should dispatch once only when both toggles are enabled, without retrying errors"
            );
        }
    }

    #[test]
    fn sampled_in_snapshot_records_the_exact_rate_and_draws_once() {
        let context = send_context_fixture();
        let response = response_builder()
            .body(EdgeBody::empty())
            .expect("should build response");
        let draws = std::cell::Cell::new(0);
        let builds = std::cell::Cell::new(0);

        let snapshot = sampled_access_snapshot(
            &context,
            || {
                draws.set(draws.get() + 1);
                0.0
            },
            || {
                builds.set(builds.get() + 1);
                build_access_telemetry_snapshot(&response, &context)
            },
        )
        .expect("should include zero entropy at a positive rate");

        assert_eq!(draws.get(), 1, "should sample exactly once");
        assert_eq!(builds.get(), 1, "should build one sampled snapshot");
        assert_eq!(snapshot.sample_rate, context.access_sample_rate);
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
        assert_eq!(snapshot.publisher_domain, "publisher.example.com");
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
    fn entry_point_404_finalization_writes_geo_state_before_access_snapshot() {
        let settings = Settings::default();
        let timings = RequestTimings::new();
        let resolved_geo = trusted_server_core::platform::GeoInfo {
            city: String::new(),
            country: "US".to_owned(),
            continent: "NorthAmerica".to_owned(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: None,
            asn: None,
        };

        for (geo_state, expected_country) in [
            (GeoLookupState::NotAttempted, "unknown"),
            (GeoLookupState::Resolved(resolved_geo), "US"),
        ] {
            let mut response = response_builder()
                .status(404)
                .body(EdgeBody::empty())
                .expect("should build router 404");

            apply_entry_point_finalize_headers(
                &settings,
                &mut response,
                None,
                &geo_state,
                &timings,
            );

            match (geo_state, response.extensions().get::<GeoLookupState>()) {
                (GeoLookupState::NotAttempted, Some(GeoLookupState::Attempted)) => {}
                (GeoLookupState::Resolved(expected), Some(GeoLookupState::Resolved(actual))) => {
                    assert_eq!(actual.country, expected.country)
                }
                (_, actual) => {
                    panic!("should write back the entry-point geo result, got {actual:?}")
                }
            }
            let snapshot = build_access_telemetry_snapshot(&response, &send_context_fixture());
            assert_eq!(
                snapshot.status, 404,
                "should preserve the router 404 status"
            );
            assert_eq!(snapshot.country, expected_country);
        }
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
    fn failed_stream_drives_emit_partial_or_error_rows_with_actual_bytes() {
        for (prefix, expected) in [(&b"example"[..], "partial"), (&b""[..], "error")] {
            let mut chunks = Vec::new();
            if !prefix.is_empty() {
                chunks.push(Ok(bytes::Bytes::copy_from_slice(prefix)));
            }
            chunks.push(Err(std::io::Error::other("example stream failure")));
            let body = EdgeBody::from_stream(futures::stream::iter(chunks));
            let timings = RequestTimings::new();
            let (writer, drive_result) = drive_streaming_body(body, Vec::new(), &timings);
            assert!(drive_result.is_err(), "should observe the stream failure");
            let result = classify_stream_delivery(&drive_result, writer.bytes());
            let snapshot = AccessTelemetrySnapshot {
                body_mode: "streamed",
                ..sample_access_snapshot()
            };
            let row: serde_json::Value =
                serde_json::from_str(&access_event_row(&snapshot, &timings.snapshot(), result, 0))
                    .expect("should parse the failed delivery row");
            assert_eq!(row["delivery_result"], expected);
            assert_eq!(row["resp_bytes"], prefix.len());
            assert_eq!(row["status"], 200, "should retain the already-sent status");
        }
    }

    #[test]
    fn stream_finish_results_reach_the_access_row() {
        for (finish_result, expected) in [
            (Ok(()), "complete"),
            (
                Err(std::io::Error::other("example finish failure")),
                "partial",
            ),
        ] {
            let result = classify_stream_finish(finish_result, "");
            let row: serde_json::Value = serde_json::from_str(&access_event_row(
                &sample_access_snapshot(),
                &Default::default(),
                result,
                0,
            ))
            .expect("should parse the transport finish row");
            assert_eq!(row["delivery_result"], expected);
        }
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

    #[test]
    fn post_send_order_is_elapsed_then_pull_sync_then_telemetry() {
        // The full contract sequence, instrumented through the real seams:
        // `send_edgezero_response` stamps `request_elapsed` before
        // returning, and `run_post_send_steps` (which every production
        // site with both steps routes through) owns pull-sync-then-
        // telemetry ordering.
        let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let timings = RequestTimings::new();
        let response = response_builder()
            .body(EdgeBody::from("ok"))
            .expect("should build response");

        let outcome = send_edgezero_response(
            response,
            None,
            &SendContext {
                timings: timings.clone(),
                server_timing_enabled: false,
                method: "GET",
                publisher_domain: "publisher.example.com",
                access_sample_rate: 1.0,
                access_telemetry_enabled: true,
            },
            None,
        );
        assert!(
            timings.snapshot().request_elapsed_ms.is_some(),
            "request_elapsed should be stamped before any post-send step runs"
        );
        assert!(
            outcome.snapshot.is_some(),
            "the access snapshot should exist for the enabled context"
        );

        let pull_log = Arc::clone(&log);
        let emit_log = Arc::clone(&log);
        run_post_send_steps(
            move || {
                pull_log
                    .lock()
                    .expect("should lock order log")
                    .push("pull_sync")
            },
            move |_| {
                emit_log
                    .lock()
                    .expect("should lock order log")
                    .push("telemetry")
            },
        );

        assert_eq!(
            *log.lock().expect("should lock order log"),
            vec!["pull_sync", "telemetry"],
            "pull-sync must dispatch before telemetry emits"
        );
    }
    /// End-to-end proof of the one-write invariant, the reads and G10 and
    /// G13 (spec 13). Every test routes a request through the real router,
    /// finalizes and sends it, and runs phase 4, against one counting store.
    mod identity_writes {
        use super::*;
        use std::collections::BTreeMap;
        use std::sync::Mutex;

        use trusted_server_core::ec::identity::IdentityProviderError;
        use trusted_server_core::ec::identity::test_support::{
            MULTI_ENTRY_SOURCE, MultiEntryTestProvider, WriteCountingEcKv, allowing_consent,
        };
        use trusted_server_core::ec::kv_backend::EcKvStore as _;
        use trusted_server_core::ec::kv_types::{
            KvEid, KvEntry, KvNetwork, KvOptOut, KvPartnerId, KvProviderState, KvUid,
        };
        use trusted_server_core::integrations::IntegrationRegistration;
        use trusted_server_core::platform::{
            GeoInfo, PlatformError, PlatformGeo, PlatformHttpClient, PlatformHttpRequest,
            PlatformPendingRequest, PlatformResponse, PlatformSelectResult,
        };
        use trusted_server_core::settings::{EcPartner, ProviderMode, ProviderSettings};

        /// The host the publisher origin answers on.
        const ORIGIN_HOST: &str = "origin.test-publisher.com";

        /// A cookie value no test treats as a real ID.
        const SHARED_ID: &str = "shared-id-1";

        /// One answer of the scripted network.
        #[derive(Clone)]
        struct Reply {
            status: u16,
            headers: Vec<(String, String)>,
            body: Vec<u8>,
        }

        /// One request the scripted network received.
        #[derive(Clone)]
        struct Seen {
            host: String,
            headers: edgezero_core::http::HeaderMap,
            body: Vec<u8>,
        }

        struct Pending {
            reply: Reply,
            backend_name: String,
        }

        /// A network that answers per host and records what it was sent. It
        /// stands in for the publisher origin, the bidders and the pull
        /// partners.
        struct ScriptedNetwork {
            replies: Mutex<BTreeMap<String, Reply>>,
            seen: Mutex<Vec<Seen>>,
        }

        impl ScriptedNetwork {
            fn new() -> Self {
                Self {
                    replies: Mutex::new(BTreeMap::new()),
                    seen: Mutex::new(Vec::new()),
                }
            }

            fn respond(&self, host: &str, status: u16, content_type: &str, body: &str) {
                self.respond_with_headers(host, status, &[("content-type", content_type)], body);
            }

            fn respond_with_headers(
                &self,
                host: &str,
                status: u16,
                headers: &[(&str, &str)],
                body: &str,
            ) {
                self.replies
                    .lock()
                    .expect("should lock the replies")
                    .insert(
                        host.to_owned(),
                        Reply {
                            status,
                            headers: headers
                                .iter()
                                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                                .collect(),
                            body: body.as_bytes().to_vec(),
                        },
                    );
            }

            /// Forgets the answer for `host`, so a call to it fails in transport.
            fn clear_reply(&self, host: &str) {
                self.replies
                    .lock()
                    .expect("should lock the replies")
                    .remove(host);
            }

            /// Every request that went to `host`, in order.
            fn requests_to(&self, host: &str) -> Vec<Seen> {
                self.seen
                    .lock()
                    .expect("should lock the requests")
                    .iter()
                    .filter(|seen| seen.host == host)
                    .cloned()
                    .collect()
            }

            /// Records `request` and returns its answer with the backend it named.
            fn record(
                &self,
                request: PlatformHttpRequest,
            ) -> Result<(Reply, String), Report<PlatformError>> {
                let backend_name = request.backend_name;
                let (parts, body) = request.request.into_parts();
                let host = parts.uri.host().unwrap_or_default().to_owned();
                self.seen
                    .lock()
                    .expect("should lock the requests")
                    .push(Seen {
                        host: host.clone(),
                        headers: parts.headers,
                        body: body
                            .into_bytes()
                            .map(|bytes| bytes.to_vec())
                            .unwrap_or_default(),
                    });
                self.replies
                    .lock()
                    .expect("should lock the replies")
                    .get(&host)
                    .cloned()
                    .map(|reply| (reply, backend_name))
                    .ok_or_else(|| Report::new(PlatformError::HttpClient))
            }
        }

        fn platform_response(
            reply: Reply,
            backend_name: String,
        ) -> Result<PlatformResponse, Report<PlatformError>> {
            let mut builder = edgezero_core::http::response_builder().status(reply.status);
            for (name, value) in reply.headers {
                builder = builder.header(name, value);
            }
            builder
                .body(EdgeBody::from(reply.body))
                .map(|response| PlatformResponse::new(response).with_backend_name(backend_name))
                .map_err(|_| Report::new(PlatformError::HttpClient))
        }

        #[async_trait::async_trait(?Send)]
        impl PlatformHttpClient for ScriptedNetwork {
            async fn send(
                &self,
                request: PlatformHttpRequest,
            ) -> Result<PlatformResponse, Report<PlatformError>> {
                let (reply, backend_name) = self.record(request)?;
                platform_response(reply, backend_name)
            }

            async fn send_async(
                &self,
                request: PlatformHttpRequest,
            ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
                let (reply, backend_name) = self.record(request)?;
                Ok(PlatformPendingRequest::new(Pending {
                    reply,
                    backend_name: backend_name.clone(),
                })
                .with_backend_name(backend_name))
            }

            async fn select(
                &self,
                mut pending_requests: Vec<PlatformPendingRequest>,
            ) -> Result<PlatformSelectResult, Report<PlatformError>> {
                if pending_requests.is_empty() {
                    return Err(Report::new(PlatformError::HttpClient));
                }
                let first = pending_requests.remove(0);
                let ready = match first.downcast::<Pending>() {
                    Ok(pending) => platform_response(pending.reply, pending.backend_name),
                    Err(_) => Err(Report::new(PlatformError::HttpClient)),
                };
                Ok(PlatformSelectResult {
                    ready,
                    remaining: pending_requests,
                    failed_backend_name: None,
                })
            }
        }

        /// Locates every client in a country no privacy law of the default
        /// consent config covers, so EC and EID consent is granted.
        struct NonRegulatedGeo;

        impl PlatformGeo for NonRegulatedGeo {
            fn lookup(
                &self,
                _client_ip: Option<std::net::IpAddr>,
            ) -> Result<Option<GeoInfo>, Report<PlatformError>> {
                Ok(Some(GeoInfo {
                    city: "Example City".to_owned(),
                    country: "CA".to_owned(),
                    continent: "NA".to_owned(),
                    latitude: 0.0,
                    longitude: 0.0,
                    metro_code: 0,
                    region: None,
                    asn: None,
                }))
            }
        }

        /// What every scenario shares: the router state, the counting store
        /// behind its identity graph and the scripted network.
        struct Scenario {
            state: Arc<AppState>,
            store: Arc<WriteCountingEcKv>,
            network: Arc<ScriptedNetwork>,
        }

        /// The partner of `source`, registered for the bidstream with no pull.
        fn partner_toml(source: &str) -> String {
            format!(
                r#"
                [[ec.partners]]
                name = "Partner {source}"
                source_domain = "{source}"
                bidstream_enabled = true
                api_token = "token-for-{source}-0123456789abcdef"
                "#
            )
        }

        impl Scenario {
            /// A scenario whose settings are the test settings plus `extra`
            /// TOML, with `sharedid.org` and `id5-sync.com` registered.
            fn with_settings(extra: &str) -> Self {
                let toml = format!(
                    "{TEST_SETTINGS_TOML}{}{}{extra}",
                    partner_toml("sharedid.org"),
                    partner_toml("id5-sync.com"),
                );
                Self::from_toml(&toml)
            }

            fn new() -> Self {
                Self::with_settings("")
            }

            fn from_toml(toml: &str) -> Self {
                let (graph, store) = KvIdentityGraph::write_counting("identity-writes-store");
                Self::from_parts(toml, graph, store)
            }

            /// A scenario over `graph`, which must count into `store`.
            fn from_parts(
                toml: &str,
                graph: KvIdentityGraph,
                store: Arc<WriteCountingEcKv>,
            ) -> Self {
                let mut settings = Settings::from_toml(toml).expect("should parse the settings");
                // These scenarios exercise the mirror; its default-off gate
                // has its own test.
                settings.ec.identity.mirror_enabled = true;
                let network = Arc::new(ScriptedNetwork::new());
                network.respond(
                    ORIGIN_HOST,
                    200,
                    "text/html",
                    "<html><head></head><body>page</body></html>",
                );
                let state = crate::app::app_state_with_network_for_test(
                    settings,
                    graph,
                    Arc::clone(&network) as Arc<dyn PlatformHttpClient>,
                    Arc::new(NonRegulatedGeo),
                );
                Self {
                    state,
                    store,
                    network,
                }
            }

            fn graph(&self) -> &KvIdentityGraph {
                self.state
                    .identity_graph
                    .as_ref()
                    .expect("should hold the graph")
            }

            /// Stores `entry` under [`EC_ID`] and zeroes the counts.
            fn seed(&self, entry: &KvEntry) {
                self.graph()
                    .create(EC_ID, entry)
                    .expect("should seed the row");
                self.store.reset_counts();
            }

            /// Seeds a live row that already holds its cluster size, so the
            /// flush has no change of its own to write (C2).
            fn seed_live(&self) -> KvEntry {
                let entry = live_entry();
                self.seed(&entry);
                entry
            }

            /// Drives `request`, and checks that no response of any scenario
            /// sets a server identity cookie (spec 13).
            fn drive(&self, request: HttpRequest) -> (HttpResponse, Option<FlushOutcome>) {
                let (response, outcome) = drive_request_for_test(&self.state, request);
                assert_no_server_identity_cookie(&response);
                (response, outcome)
            }

            /// The row under `ec_id`.
            fn row_of(&self, ec_id: &str) -> KvEntry {
                self.graph()
                    .get(ec_id)
                    .expect("should read the row")
                    .expect("should hold the row")
                    .0
            }

            /// The row under [`EC_ID`].
            fn row(&self) -> KvEntry {
                self.row_of(EC_ID)
            }
        }

        /// A live row for [`EC_ID`] with its cluster size present.
        fn live_entry() -> KvEntry {
            let mut entry = KvEntry::new(&allowing_consent(), None, 1_000, "test-publisher.com");
            entry.network = Some(KvNetwork {
                cluster_size: Some(1),
            });
            entry
        }

        fn with_header(mut request: HttpRequest, name: &'static str, value: &str) -> HttpRequest {
            request.headers_mut().insert(
                name,
                HeaderValue::from_str(value).expect("should encode the header"),
            );
            request
        }

        fn without_header(mut request: HttpRequest, name: &'static str) -> HttpRequest {
            request.headers_mut().remove(name);
            request
        }

        /// The cookies every response of every scenario must leave unset:
        /// the server sets `ts-ec` and no other identity cookie.
        fn is_server_identity_cookie(name: &str) -> bool {
            name == "ts-eids"
                || name == "__Host-ts-eids-s"
                || name.starts_with("ts-eids-r")
                || name == "ts-ec-pull-complete"
        }

        fn cookie_name(set_cookie: &str) -> &str {
            set_cookie.split('=').next().unwrap_or_default().trim()
        }

        fn assert_no_server_identity_cookie(response: &HttpResponse) {
            for cookie in set_cookies(response) {
                assert!(
                    !is_server_identity_cookie(cookie_name(&cookie)),
                    "should set no identity cookie but ts-ec: {cookie}"
                );
            }
        }

        /// The value of the `name` cookie the response sets.
        fn set_cookie_value(response: &HttpResponse, name: &str) -> Option<String> {
            set_cookies(response).into_iter().find_map(|cookie| {
                let pair = cookie.split(';').next()?;
                let (found, value) = pair.split_once('=')?;
                (found.trim() == name).then(|| value.trim().to_owned())
            })
        }

        /// The cookie of a returning visitor whose browser holds a sharedId.
        fn returning_with_shared_id() -> String {
            format!("ts-ec={EC_ID}; sharedId={SHARED_ID}")
        }

        /// Asserts every write mode count, so no mode hides behind another.
        fn assert_writes(store: &WriteCountingEcKv, expected: (usize, usize, usize), what: &str) {
            let writes = store.writes();
            assert_eq!(
                (writes.add, writes.if_generation_match, writes.overwrite),
                expected,
                "should make exactly (Add, IfGenerationMatch, Overwrite) writes: {what}"
            );
        }

        fn assert_no_writes(store: &WriteCountingEcKv, what: &str) {
            assert_writes(store, (0, 0, 0), what);
        }

        /// The UID the row holds for `source`.
        fn stored_uid(row: &KvEntry, source: &str) -> Option<String> {
            row.ids
                .get(source)
                .and_then(KvPartnerId::first_uid)
                .map(str::to_owned)
        }

        #[test]
        fn returning_visitor_with_changes_writes_once() {
            let scenario = Scenario::new();
            scenario.seed_live();

            let (response, outcome) = scenario.drive(navigation(Some(&returning_with_shared_id())));

            assert_eq!(response.status(), 200, "should serve the origin page");
            assert_eq!(outcome, Some(FlushOutcome::Written), "should flush");
            assert_writes(&scenario.store, (0, 1, 0), "one CAS write");
            assert_eq!(
                stored_uid(&scenario.row(), "sharedid.org").as_deref(),
                Some(SHARED_ID),
                "should hold the browser's sharedId in KV after the one write"
            );
        }

        /// A store that refuses its first `collisions` `Add` writes without
        /// storing them, as a minted ID that is already taken does.
        struct CollidingStore {
            inner: Arc<WriteCountingEcKv>,
            collisions: Mutex<u32>,
            refused: Arc<std::sync::atomic::AtomicUsize>,
        }

        impl trusted_server_core::ec::kv_backend::EcKvStore for CollidingStore {
            fn store_name(&self) -> &str {
                self.inner.store_name()
            }

            fn lookup(
                &self,
                key: &str,
            ) -> Result<
                Option<trusted_server_core::ec::kv_backend::EcKvLookup>,
                Report<TrustedServerError>,
            > {
                self.inner.lookup(key)
            }

            fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
                self.inner.key_exists(key)
            }

            fn insert(
                &self,
                key: &str,
                write: trusted_server_core::ec::kv_backend::EcKvWrite<'_>,
            ) -> Result<
                trusted_server_core::ec::kv_backend::EcKvWriteOutcome,
                Report<TrustedServerError>,
            > {
                if matches!(
                    write.mode,
                    trusted_server_core::ec::kv_backend::EcKvWriteMode::Add
                ) {
                    let mut remaining = self.collisions.lock().expect("should lock the count");
                    if *remaining > 0 {
                        *remaining -= 1;
                        self.refused
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        return Ok(
                            trusted_server_core::ec::kv_backend::EcKvWriteOutcome::PreconditionFailed,
                        );
                    }
                }
                self.inner.insert(key, write)
            }

            fn list_keys_with_prefix(
                &self,
                prefix: &str,
                limit: u32,
            ) -> Result<Vec<String>, Report<TrustedServerError>> {
                self.inner.list_keys_with_prefix(prefix, limit)
            }

            fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
                self.inner.delete(key)
            }
        }

        /// The same navigation with one header replaced.
        fn navigation_with_header(
            cookie: Option<&str>,
            name: &'static str,
            value: &str,
        ) -> HttpRequest {
            with_header(navigation(cookie), name, value)
        }

        #[test]
        fn identity_measurement_counts_the_create_write_of_a_new_ec_navigation() {
            let scenario = Scenario::new();
            let cookie = format!("sharedId={SHARED_ID}");

            let (outcome, measurement) =
                drive_measured_for_test(&scenario.state, navigation(Some(&cookie)));

            assert_eq!(
                measurement.route(),
                RouteKind::NewEc,
                "should label a first visit as a new EC"
            );
            assert_eq!(
                measurement.kv_writes(),
                1,
                "should count the create write and no other"
            );
            assert_eq!(
                measurement.kv_reads(),
                0,
                "should count no read: the create reads nothing"
            );
            assert_eq!(
                outcome,
                Some(FlushOutcome::Skipped),
                "should not flush a new visitor"
            );
        }

        #[test]
        fn new_visitor_navigation_writes_once() {
            let scenario = Scenario::new();
            let cookie = format!("sharedId={SHARED_ID}");

            let (response, outcome) = scenario.drive(navigation(Some(&cookie)));

            assert_eq!(response.status(), 200, "should serve the origin page");
            let minted = set_cookie_value(&response, "ts-ec").expect("should mint an EC");
            assert_eq!(
                outcome,
                Some(FlushOutcome::Skipped),
                "should leave a new visitor nothing to flush"
            );
            assert_writes(&scenario.store, (1, 0, 0), "one Add and no flush");
            assert_eq!(
                stored_uid(&scenario.row_of(&minted), "sharedid.org").as_deref(),
                Some(SHARED_ID),
                "should carry the browser ingest in the create"
            );
        }

        #[test]
        fn new_visitor_collision_remints_and_writes_once() {
            let (_, counting) = KvIdentityGraph::write_counting("collision-store");
            let refused = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let graph = KvIdentityGraph::new(CollidingStore {
                inner: Arc::clone(&counting),
                collisions: Mutex::new(1),
                refused: Arc::clone(&refused),
            });
            let toml = format!("{TEST_SETTINGS_TOML}{}", partner_toml("sharedid.org"));
            let scenario = Scenario::from_parts(&toml, graph, counting);
            let cookie = format!("sharedId={SHARED_ID}");

            let (response, _) = scenario.drive(navigation(Some(&cookie)));

            assert_eq!(
                refused.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "control: should have met one taken ID first"
            );
            assert_writes(
                &scenario.store,
                (1, 0, 0),
                "one Add lands after the collision",
            );
            let minted: Vec<String> = set_cookies(&response)
                .into_iter()
                .filter(|cookie| cookie_name(cookie) == "ts-ec")
                .collect();
            assert_eq!(
                minted.len(),
                1,
                "should set one ts-ec, for the ID that landed"
            );
            let id = set_cookie_value(&response, "ts-ec").expect("should mint an EC");
            assert_eq!(
                stored_uid(&scenario.row_of(&id), "sharedid.org").as_deref(),
                Some(SHARED_ID),
                "should hold the ingest under the re-minted ID"
            );
        }

        #[test]
        fn returning_visitor_without_changes_writes_nothing() {
            let scenario = Scenario::new();
            let mut entry = live_entry();
            entry.ids.insert(
                "sharedid.org".to_owned(),
                KvPartnerId::browser(SHARED_ID, Some(1_500)),
            );
            scenario.seed(&entry);

            let (response, outcome) = scenario.drive(navigation(Some(&returning_with_shared_id())));

            assert_eq!(response.status(), 200, "should serve the origin page");
            assert_eq!(
                outcome,
                Some(FlushOutcome::NoChange),
                "should find the row already holds the ID"
            );
            assert_no_writes(&scenario.store, "the browser's ID is already stored");

            let (_, nothing) = scenario.drive(navigation(Some(&format!("ts-ec={EC_ID}"))));
            assert_eq!(
                nothing,
                Some(FlushOutcome::Skipped),
                "should skip a navigation that carries nothing"
            );
            assert_no_writes(&scenario.store, "nothing to ingest");

            let control = Scenario::new();
            control.seed_live();
            control.drive(navigation(Some(&returning_with_shared_id())));
            assert_writes(
                &control.store,
                (0, 1, 0),
                "control: the same request writes when the row lacks the ID",
            );
        }

        #[test]
        fn revalidation_304_with_changes_writes_once() {
            let scenario = Scenario::new();
            scenario.seed_live();
            scenario
                .network
                .respond_with_headers(ORIGIN_HOST, 304, &[("etag", "\"page-1\"")], "");
            let request = navigation_with_header(
                Some(&returning_with_shared_id()),
                "if-none-match",
                "\"page-1\"",
            );

            let (response, outcome) = scenario.drive(request);

            assert_eq!(response.status(), 304, "should answer the revalidation");
            assert_eq!(outcome, Some(FlushOutcome::Written), "should flush");
            assert_writes(&scenario.store, (0, 1, 0), "one CAS write");
            assert_eq!(
                stored_uid(&scenario.row(), "sharedid.org").as_deref(),
                Some(SHARED_ID),
                "should hold the sharedId"
            );
        }

        #[test]
        fn returning_navigation_answered_3xx_4xx_5xx_writes_nothing() {
            let control = Scenario::new();
            control.seed_live();
            control.drive(navigation(Some(&returning_with_shared_id())));
            assert_writes(&control.store, (0, 1, 0), "control: a 200 writes once");

            for status in [301_u16, 302, 400, 404, 410, 500, 502, 503] {
                let scenario = Scenario::new();
                scenario.seed_live();
                scenario.network.respond_with_headers(
                    ORIGIN_HOST,
                    status,
                    &[
                        ("content-type", "text/html"),
                        ("location", "https://origin.test-publisher.com/next"),
                    ],
                    "<html><head></head><body>answer</body></html>",
                );

                let (response, outcome) =
                    scenario.drive(navigation(Some(&returning_with_shared_id())));

                assert_eq!(
                    response.status().as_u16(),
                    status,
                    "should pass the origin status through: {status}"
                );
                assert_eq!(
                    outcome,
                    Some(FlushOutcome::Skipped),
                    "should have nothing to flush after {status}"
                );
                assert_no_writes(&scenario.store, &format!("origin answered {status}"));
                assert_eq!(
                    stored_uid(&scenario.row(), "sharedid.org"),
                    None,
                    "should not store the ID the browser sent, after {status}"
                );
            }
        }

        #[test]
        fn new_visitor_prefetch_writes_once() {
            let scenario = Scenario::new();

            let (response, _) = scenario.drive(navigation_with_header(
                Some(&format!("sharedId={SHARED_ID}")),
                "sec-purpose",
                "prefetch",
            ));

            assert!(
                set_cookie_value(&response, "ts-ec").is_some(),
                "should mint an EC for a prefetched navigation"
            );
            assert_writes(&scenario.store, (1, 0, 0), "one Add");
        }

        #[test]
        fn returning_prefetch_writes_nothing() {
            for header in ["sec-purpose", "purpose"] {
                let scenario = Scenario::new();
                scenario.seed_live();

                let (_, outcome) = scenario.drive(navigation_with_header(
                    Some(&returning_with_shared_id()),
                    header,
                    "prefetch",
                ));

                assert_eq!(
                    outcome,
                    Some(FlushOutcome::Skipped),
                    "should not be the writer: {header}"
                );
                assert_no_writes(&scenario.store, header);
            }

            let control = Scenario::new();
            control.seed_live();
            control.drive(navigation(Some(&returning_with_shared_id())));
            assert_writes(
                &control.store,
                (0, 1, 0),
                "control: the same navigation without prefetch writes once",
            );
        }

        #[test]
        fn new_visitor_without_sec_fetch_dest_writes_once() {
            let scenario = Scenario::new();
            let request = with_header(
                without_header(
                    navigation(Some(&format!("sharedId={SHARED_ID}"))),
                    "sec-fetch-dest",
                ),
                "accept",
                "text/html,application/xhtml+xml",
            );

            let (response, _) = scenario.drive(request);

            assert!(
                set_cookie_value(&response, "ts-ec").is_some(),
                "should mint an EC"
            );
            assert_writes(&scenario.store, (1, 0, 0), "one Add");
        }

        #[test]
        fn returning_iframe_or_no_sec_fetch_dest_writes_nothing() {
            let requests = [
                (
                    "iframe",
                    navigation_with_header(
                        Some(&returning_with_shared_id()),
                        "sec-fetch-dest",
                        "iframe",
                    ),
                ),
                (
                    "no Sec-Fetch-Dest",
                    with_header(
                        without_header(
                            navigation(Some(&returning_with_shared_id())),
                            "sec-fetch-dest",
                        ),
                        "accept",
                        "text/html",
                    ),
                ),
            ];
            for (name, request) in requests {
                let scenario = Scenario::new();
                scenario.seed_live();

                let (_, outcome) = scenario.drive(request);

                assert_eq!(
                    outcome,
                    Some(FlushOutcome::Skipped),
                    "should not be the writer: {name}"
                );
                assert_no_writes(&scenario.store, name);
            }

            let control = Scenario::new();
            control.seed_live();
            control.drive(navigation(Some(&returning_with_shared_id())));
            assert_writes(
                &control.store,
                (0, 1, 0),
                "control: a document navigation writes once",
            );
        }

        /// The settings of an enabled Didomi proxy, whose upstream answers on
        /// `sdk.example.com`.
        const DIDOMI_TOML: &str = r#"
            [integrations.didomi]
            enabled = true
            sdk_origin = "https://sdk.example.com"
            api_origin = "https://api.example.com"
            "#;

        fn didomi_scenario() -> Scenario {
            let scenario = Scenario::with_settings(DIDOMI_TOML);
            scenario
                .network
                .respond("sdk.example.com", 200, "application/javascript", "loader()");
            scenario
        }

        fn didomi_navigation(cookie: Option<&str>) -> HttpRequest {
            let mut request = navigation(cookie);
            *request.uri_mut() = "https://test-publisher.com/integrations/didomi/consent/loader.js"
                .parse()
                .expect("should parse the proxy URI");
            request
        }

        #[test]
        fn integration_proxy_navigation_minting_ec_writes_once() {
            let scenario = didomi_scenario();

            let (response, _) =
                scenario.drive(didomi_navigation(Some(&format!("sharedId={SHARED_ID}"))));

            assert_eq!(response.status(), 200, "should serve the proxied answer");
            assert_eq!(
                scenario.network.requests_to("sdk.example.com").len(),
                1,
                "control: should have reached the integration, not the publisher"
            );
            assert!(
                scenario.network.requests_to(ORIGIN_HOST).is_empty(),
                "should not reach the publisher origin"
            );
            let minted = set_cookie_value(&response, "ts-ec").expect("should mint an EC");
            assert_writes(&scenario.store, (1, 0, 0), "one Add");
            assert_eq!(
                stored_uid(&scenario.row_of(&minted), "sharedid.org").as_deref(),
                Some(SHARED_ID),
                "should carry the ingest in the create"
            );
        }

        /// An enabled auction with one bidder, and one slot for page bids.
        const AUCTION_TOML: &str = r#"
            [auction]
            enabled = true
            timeout_ms = 2000

            [auction.providers.bidder-main]
            protocol = "openrtb-2.6"
            profile = "standard"
            endpoint = "https://bidder.example.com/auction"
            routing = "all_eligible"

            [creative_opportunities]
            enabled = true
            gam_network_id = "123456789"

            [[creative_opportunities.slot]]
            id = "leaderboard"
            gam_unit_path = "/123456789/leaderboard"
            div_id = "div-gpt-ad-leaderboard"
            page_patterns = ["/"]
            formats = [{ width = 728, height = 90 }]
            "#;

        /// A first-party asset route to a scripted origin.
        const ASSET_TOML: &str = r#"
            [[proxy.asset_routes]]
            prefix = "/assets/"
            origin_url = "https://assets.example.com"
            "#;

        /// A pull partner whose answers the scripted network serves.
        const PULL_PARTNER_TOML: &str = r#"
            [[ec.partners]]
            name = "Pull SSP"
            source_domain = "ssp.example.com"
            bidstream_enabled = true
            pull_sync_enabled = true
            pull_sync_url = "https://sync.ssp.example.com/pull"
            pull_sync_allowed_domains = ["sync.ssp.example.com"]
            ts_pull_token = "example-token-value-0123456789abcdef"
            "#;

        /// The provider the pull partner becomes.
        const PULL_PROVIDER: &str = "pull-ssp-example-com";

        const BIDDER_HOST: &str = "bidder.example.com";
        const PULL_HOST: &str = "sync.ssp.example.com";

        /// A scenario with the auction, the Didomi proxy, an asset route and
        /// the pull partner, and a network that answers all of them.
        fn full_scenario() -> Scenario {
            let scenario = Scenario::with_settings(&format!(
                "{AUCTION_TOML}{DIDOMI_TOML}{ASSET_TOML}{PULL_PARTNER_TOML}"
            ));
            scenario.network.respond(
                BIDDER_HOST,
                200,
                "application/json",
                r#"{"id":"response","seatbid":[]}"#,
            );
            scenario
                .network
                .respond("sdk.example.com", 200, "application/javascript", "loader()");
            scenario.network.respond(
                "assets.example.com",
                200,
                "application/javascript",
                "asset()",
            );
            scenario.network.respond(
                PULL_HOST,
                200,
                "application/json",
                r#"{"uid":"pull-uid-1"}"#,
            );
            scenario
        }

        /// A scenario with the auction and no pull partner.
        fn auction_scenario() -> Scenario {
            let scenario = Scenario::with_settings(AUCTION_TOML);
            scenario.network.respond(
                BIDDER_HOST,
                200,
                "application/json",
                r#"{"id":"response","seatbid":[]}"#,
            );
            scenario
        }

        fn auction_body(eids: Option<&serde_json::Value>) -> serde_json::Value {
            let mut body = serde_json::json!({
                "adUnits": [{
                    "code": "div-1",
                    "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
                }]
            });
            if let Some(eids) = eids {
                body["eids"] = eids.clone();
            }
            body
        }

        /// A `/auction` request from a real browser.
        fn auction_request(cookie: Option<&str>, body: &serde_json::Value) -> HttpRequest {
            let mut builder = edgezero_core::http::request_builder()
                .method(edgezero_core::http::Method::POST)
                .uri("https://test-publisher.com/auction")
                .header("content-type", "application/json");
            if let Some(cookie) = cookie {
                builder = builder.header("cookie", cookie);
            }
            let mut request = builder
                .body(EdgeBody::from(
                    serde_json::to_vec(body).expect("should serialize the body"),
                ))
                .expect("should build request");
            as_browser(&mut request);
            request
        }

        /// A browser's `/_ts/page-bids` call for the home page.
        fn page_bids_request(cookie: &str) -> HttpRequest {
            let mut request = edgezero_core::http::request_builder()
                .method(edgezero_core::http::Method::GET)
                .uri("https://test-publisher.com/_ts/page-bids?path=%2F")
                .header("x-tsjs-page-bids", "1")
                .header("cookie", cookie)
                .body(EdgeBody::empty())
                .expect("should build request");
            as_browser(&mut request);
            request
        }

        /// The partner `source` asking who the visitor is.
        fn identify_request(cookie: &str, source: &str) -> HttpRequest {
            let mut request = edgezero_core::http::request_builder()
                .method(edgezero_core::http::Method::GET)
                .uri("https://test-publisher.com/_ts/api/v1/identify")
                .header(
                    "authorization",
                    format!("Bearer token-for-{source}-0123456789abcdef"),
                )
                .header("cookie", cookie)
                .header("origin", "https://www.test-publisher.com")
                .header("sec-fetch-site", "same-site")
                .body(EdgeBody::empty())
                .expect("should build request");
            as_browser(&mut request);
            request
        }

        fn asset_request(cookie: &str) -> HttpRequest {
            let mut request = edgezero_core::http::request_builder()
                .method(edgezero_core::http::Method::GET)
                .uri("https://test-publisher.com/assets/app.js")
                .header("sec-fetch-dest", "script")
                .header("cookie", cookie)
                .body(EdgeBody::empty())
                .expect("should build request");
            as_browser(&mut request);
            request
        }

        fn json_of(response: HttpResponse) -> serde_json::Value {
            let bytes = response
                .into_body()
                .into_bytes()
                .expect("should buffer the body");
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        }

        /// The last request body the bidder received.
        fn bidder_body(scenario: &Scenario) -> serde_json::Value {
            let seen = scenario
                .network
                .requests_to(BIDDER_HOST)
                .pop()
                .expect("should have called the bidder");
            serde_json::from_slice(&seen.body).expect("should parse the bidder request")
        }

        /// The EIDs of a bidder request, from `user.ext.eids`.
        fn bidder_eids(body: &serde_json::Value) -> serde_json::Value {
            body["user"]["ext"]["eids"].clone()
        }

        /// What `x-ts-eids` carries.
        fn header_eids(response: &HttpResponse) -> serde_json::Value {
            use base64::Engine as _;
            let value = response
                .headers()
                .get("x-ts-eids")
                .and_then(|value| value.to_str().ok())
                .expect("should carry x-ts-eids");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(value)
                .expect("should decode x-ts-eids");
            serde_json::from_slice(&bytes).expect("should parse x-ts-eids")
        }

        /// The decoded `ts-eids-s` payload of an `/auction` body, or its
        /// `clear` marker.
        fn mirror_of(body: &serde_json::Value) -> serde_json::Value {
            use base64::Engine as _;
            let member = &body["ext"]["ts_eids_s"];
            match member["value"].as_str() {
                Some(value) => {
                    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(value)
                        .expect("should decode the mirror");
                    serde_json::from_slice(&bytes).expect("should parse the mirror")
                }
                None => member.clone(),
            }
        }

        #[test]
        fn auction_page_bids_identify_proxy_and_assets_write_nothing() {
            let scenario = full_scenario();
            scenario.seed_live();
            let cookie = returning_with_shared_id();
            let body = auction_body(None);

            let requests = [
                ("auction", auction_request(Some(&cookie), &body)),
                ("page bids", page_bids_request(&cookie)),
                ("identify", identify_request(&cookie, "sharedid.org")),
                ("integration proxy subresource", {
                    let mut request = didomi_navigation(Some(&cookie));
                    request
                        .headers_mut()
                        .insert("sec-fetch-dest", HeaderValue::from_static("script"));
                    request
                }),
                ("asset", asset_request(&cookie)),
            ];
            for (name, request) in requests {
                let (response, _) = scenario.drive(request);

                assert!(
                    response.status().is_success(),
                    "should answer {name} with a success, got {}",
                    response.status()
                );
                assert_no_writes(&scenario.store, name);
            }
            assert_eq!(
                scenario.network.requests_to(BIDDER_HOST).len(),
                2,
                "control: the auction and the page bids should both have reached the bidder"
            );
            for host in ["sdk.example.com", "assets.example.com"] {
                assert_eq!(
                    scenario.network.requests_to(host).len(),
                    1,
                    "control: the proxy and the asset route should each have reached {host}"
                );
            }

            let control = full_scenario();
            control.seed_live();
            control.drive(navigation(Some(&cookie)));
            assert_writes(
                &control.store,
                (0, 1, 0),
                "control: a navigation with the same cookies writes once",
            );
        }

        #[test]
        fn returning_writer_navigation_reads_once() {
            let changed = Scenario::new();
            changed.seed_live();

            changed.drive(navigation(Some(&returning_with_shared_id())));

            assert_eq!(
                changed.store.writes().lookups,
                1,
                "should read once, with a write"
            );
            assert_writes(&changed.store, (0, 1, 0), "one CAS write");

            let unchanged = Scenario::new();
            let mut entry = live_entry();
            entry.ids.insert(
                "sharedid.org".to_owned(),
                KvPartnerId::browser(SHARED_ID, Some(1_500)),
            );
            unchanged.seed(&entry);

            unchanged.drive(navigation(Some(&returning_with_shared_id())));

            assert_eq!(
                unchanged.store.writes().lookups,
                1,
                "should read once, with nothing to write"
            );
            assert_no_writes(&unchanged.store, "the row already holds the ID");

            unchanged.drive(navigation(Some(&returning_with_shared_id())));
            assert_eq!(
                unchanged.store.writes().lookups,
                2,
                "control: the counter should count each navigation's read"
            );
        }

        #[test]
        fn due_after_send_provider_adds_one_reread() {
            let scenario = full_scenario();
            scenario.seed_live();

            let (_, outcome) = scenario.drive(navigation(Some(&returning_with_shared_id())));

            assert_eq!(outcome, Some(FlushOutcome::Written), "should flush");
            assert_eq!(
                scenario.network.requests_to(PULL_HOST).len(),
                1,
                "should call the due pull provider after send"
            );
            assert_eq!(
                scenario.store.writes().lookups,
                2,
                "should read in phase 1 and once more before dispatching"
            );
            assert_writes(
                &scenario.store,
                (0, 1, 0),
                "the browser ingest and the pull in one write",
            );
            let row = scenario.row();
            assert_eq!(
                stored_uid(&row, "sharedid.org").as_deref(),
                Some(SHARED_ID),
                "should hold the browser ID"
            );
            assert_eq!(
                stored_uid(&row, "ssp.example.com").as_deref(),
                Some("pull-uid-1"),
                "should hold the pulled ID"
            );

            let idle = full_scenario();
            let mut entry = live_entry();
            entry.ps.insert(
                PULL_PROVIDER.to_owned(),
                trusted_server_core::ec::kv_types::KvProviderState {
                    n: Some(4_000_000_000),
                    ..Default::default()
                },
            );
            idle.seed(&entry);

            idle.drive(navigation(Some(&returning_with_shared_id())));

            assert!(
                idle.network.requests_to(PULL_HOST).is_empty(),
                "control: should not call a provider that is not due"
            );
            assert_eq!(
                idle.store.writes().lookups,
                1,
                "control: should read once when no provider is due"
            );
        }

        #[test]
        fn auction_and_page_bids_keep_one_read() {
            let cookie = returning_with_shared_id();
            let auction = full_scenario();
            auction.seed_live();

            auction.drive(auction_request(Some(&cookie), &auction_body(None)));

            assert_eq!(
                auction.store.writes().lookups,
                1,
                "/auction should read once"
            );
            assert_no_writes(&auction.store, "/auction");
            assert_eq!(
                auction.network.requests_to(BIDDER_HOST).len(),
                1,
                "control: the auction should have run"
            );

            let page_bids = full_scenario();
            page_bids.seed_live();

            page_bids.drive(page_bids_request(&cookie));

            assert_eq!(
                page_bids.store.writes().lookups,
                1,
                "/_ts/page-bids should read once"
            );
            assert_no_writes(&page_bids.store, "/_ts/page-bids");
            assert_eq!(
                page_bids.network.requests_to(BIDDER_HOST).len(),
                1,
                "control: the page-bids auction should have run"
            );
        }

        #[test]
        fn same_origin_page_bids_is_the_spa_writer_once_the_interval_passes() {
            let spa_request = || {
                let mut request = page_bids_request(&returning_with_shared_id());
                request.headers_mut().insert(
                    "sec-fetch-site",
                    edgezero_core::http::HeaderValue::from_static("same-origin"),
                );
                request
            };

            let due = full_scenario();
            due.seed_live();
            let (_, outcome) = due.drive(spa_request());
            assert_eq!(
                outcome,
                Some(FlushOutcome::Written),
                "should write once as the SPA writer"
            );
            assert_eq!(
                stored_uid(&due.row(), "sharedid.org").as_deref(),
                Some(SHARED_ID),
                "should ingest the browser's sharedId"
            );
            assert_eq!(
                due.network.requests_to(PULL_HOST).len(),
                1,
                "should run the due after-send provider"
            );

            let recent = full_scenario();
            let mut row = live_entry();
            row.wa = Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("should read the clock")
                    .as_secs(),
            );
            recent.seed(&row);
            let (_, outcome) = recent.drive(spa_request());
            assert_eq!(
                outcome,
                Some(FlushOutcome::Skipped),
                "should wait out the interval since the last writer flush"
            );
            assert_no_writes(&recent.store, "an SPA call within the interval");
        }

        #[test]
        fn no_response_sets_an_identity_cookie_other_than_ts_ec() {
            assert!(
                is_server_identity_cookie("ts-eids")
                    && is_server_identity_cookie("__Host-ts-eids-s")
                    && is_server_identity_cookie("ts-eids-r1")
                    && is_server_identity_cookie("ts-ec-pull-complete")
                    && !is_server_identity_cookie("ts-ec"),
                "control: the check should name exactly the cookies the server never sets"
            );
            let browser_cookies = format!(
                "{}; ts-eids=ignored; __Host-ts-eids-s=ignored; ts-eids-r1=ignored; \
                 ts-ec-pull-complete=v1.example",
                returning_with_shared_id()
            );

            let new_visitor = full_scenario();
            let (minted, _) = new_visitor.drive(navigation(Some("sharedId=shared-id-1")));
            assert!(
                set_cookie_value(&minted, "ts-ec").is_some(),
                "control: a new visitor's response should set ts-ec"
            );

            let returning = full_scenario();
            returning.seed_live();
            let body = auction_body(None);
            for request in [
                navigation(Some(&browser_cookies)),
                auction_request(Some(&browser_cookies), &body),
                page_bids_request(&browser_cookies),
                identify_request(&browser_cookies, "sharedid.org"),
            ] {
                let (response, _) = returning.drive(request);
                let names: Vec<String> = set_cookies(&response)
                    .iter()
                    .map(|cookie| cookie_name(cookie).to_owned())
                    .collect();
                assert!(
                    names.iter().all(|name| name == "ts-ec"),
                    "should set at most ts-ec: {names:?}"
                );
            }
        }

        /// A source `id5-sync.com` entry set with two entries, three UIDs and
        /// every UID-level and EID-level field.
        fn full_entry_set() -> Vec<trusted_server_core::ec::kv_types::KvEid> {
            use trusted_server_core::ec::kv_types::{KvEid, KvUid};
            vec![
                KvEid {
                    uids: vec![
                        KvUid {
                            id: "set-uid-a".to_owned(),
                            atype: Some(1),
                            ext: Some(serde_json::json!({ "rtiPartner": "A" })),
                        },
                        KvUid {
                            id: "set-uid-b".to_owned(),
                            atype: Some(3),
                            ext: Some(serde_json::json!({ "stype": "ppuid" })),
                        },
                    ],
                    inserter: Some("inserter-a.example.com".to_owned()),
                    matcher: Some("matcher-a.example.com".to_owned()),
                    mm: Some(3),
                    ext: Some(serde_json::json!({ "segments": ["s1", "s2"] })),
                },
                KvEid {
                    uids: vec![KvUid {
                        id: "set-uid-c".to_owned(),
                        atype: Some(1),
                        ext: Some(serde_json::json!({ "label": "c" })),
                    }],
                    inserter: Some("inserter-b.example.com".to_owned()),
                    matcher: Some("matcher-b.example.com".to_owned()),
                    mm: Some(1),
                    ext: Some(serde_json::json!({ "tier": "gold" })),
                },
            ]
        }

        /// What bidders receive for [`full_entry_set`].
        fn full_entry_set_eids() -> serde_json::Value {
            serde_json::json!([
                {
                    "source": "id5-sync.com",
                    "inserter": "inserter-a.example.com",
                    "matcher": "matcher-a.example.com",
                    "mm": 3,
                    "ext": { "segments": ["s1", "s2"] },
                    "uids": [
                        { "id": "set-uid-a", "atype": 1, "ext": { "rtiPartner": "A" } },
                        { "id": "set-uid-b", "atype": 3, "ext": { "stype": "ppuid" } }
                    ]
                },
                {
                    "source": "id5-sync.com",
                    "inserter": "inserter-b.example.com",
                    "matcher": "matcher-b.example.com",
                    "mm": 1,
                    "ext": { "tier": "gold" },
                    "uids": [{ "id": "set-uid-c", "atype": 1, "ext": { "label": "c" } }]
                }
            ])
        }

        /// A live row holding `eids` for `id5-sync.com`, with no writer.
        fn row_with_set(exp: Option<u64>) -> KvEntry {
            let mut entry = live_entry();
            entry.ids.insert(
                "id5-sync.com".to_owned(),
                KvPartnerId {
                    at: Some(1_500),
                    exp,
                    eids: full_entry_set(),
                    ..KvPartnerId::default()
                },
            );
            entry
        }

        /// A browser entry of a source no partner registers.
        fn browser_entry() -> serde_json::Value {
            serde_json::json!({
                "source": "browser.example.com",
                "uids": [{ "id": "browser-uid", "atype": 1 }]
            })
        }

        const FAR_FUTURE: u64 = 4_000_000_000;

        fn ec_cookie() -> String {
            format!("ts-ec={EC_ID}")
        }

        #[test]
        fn entry_set_round_trips_from_kv_to_bidders() {
            let scenario = auction_scenario();
            scenario.seed(&row_with_set(Some(FAR_FUTURE)));
            let body = auction_body(Some(&serde_json::json!([browser_entry()])));

            let (response, _) = scenario.drive(auction_request(Some(&ec_cookie()), &body));

            let mut expected = vec![browser_entry()];
            expected.extend(
                full_entry_set_eids()
                    .as_array()
                    .expect("should be a list")
                    .clone(),
            );
            let expected = serde_json::Value::Array(expected);
            assert_eq!(
                bidder_eids(&bidder_body(&scenario)),
                expected,
                "should send the browser entry, then both stored entries in order, every field intact"
            );
            assert_eq!(
                header_eids(&response),
                expected,
                "x-ts-eids should carry what was sent"
            );
            let mirror = mirror_of(&json_of(response));
            assert_eq!(
                mirror,
                serde_json::json!({ "v": 1, "eids": full_entry_set_eids() }),
                "the mirror should hold the same stored entries and no browser entry"
            );
            assert_no_writes(&scenario.store, "/auction");
        }

        #[test]
        fn entry_set_survives_an_unrelated_flush() {
            let scenario = Scenario::new();
            let seeded = row_with_set(Some(FAR_FUTURE));
            scenario.seed(&seeded);

            let (_, outcome) = scenario.drive(navigation(Some(&returning_with_shared_id())));

            assert_eq!(
                outcome,
                Some(FlushOutcome::Written),
                "should flush the sharedId"
            );
            assert_writes(&scenario.store, (0, 1, 0), "one CAS write");
            let row = scenario.row();
            assert_eq!(
                stored_uid(&row, "sharedid.org").as_deref(),
                Some(SHARED_ID),
                "control: the flush should have written the sharedId fill"
            );
            assert_eq!(
                row.ids.get("id5-sync.com"),
                seeded.ids.get("id5-sync.com"),
                "should leave the stored entry set exactly as it was"
            );
        }

        #[test]
        fn expired_entry_set_is_never_served_or_mirrored() {
            let live = auction_scenario();
            live.seed(&row_with_set(Some(FAR_FUTURE)));
            let (live_response, _) =
                live.drive(auction_request(Some(&ec_cookie()), &auction_body(None)));
            assert_eq!(
                bidder_eids(&bidder_body(&live)),
                full_entry_set_eids(),
                "control: an unexpired set should be served"
            );
            assert_ne!(
                mirror_of(&json_of(live_response)),
                serde_json::json!({ "clear": true }),
                "control: an unexpired set should be mirrored"
            );

            let expired = auction_scenario();
            expired.seed(&row_with_set(Some(1_500)));
            let (response, _) =
                expired.drive(auction_request(Some(&ec_cookie()), &auction_body(None)));

            assert_eq!(
                bidder_eids(&bidder_body(&expired)),
                serde_json::Value::Null,
                "should send no EIDs from an expired set"
            );
            assert_eq!(
                mirror_of(&json_of(response)),
                serde_json::json!({ "clear": true }),
                "should mirror nothing for an expired set"
            );
        }

        /// A version 1 row: `uid` records and no entry sets.
        fn seed_version_1_row(scenario: &Scenario) {
            scenario.seed(&live_entry());
            let lookup = scenario
                .store
                .lookup(EC_ID)
                .expect("should read the row")
                .expect("should hold the row");
            let body = serde_json::json!({
                "v": 1,
                "created": 1_000,
                "consent": { "ok": true, "updated": 1_000 },
                "geo": { "country": "CA" },
                "network": { "cluster_size": 1 },
                "ids": { "id5-sync.com": { "uid": "version-1-uid", "at": 1_500 } }
            })
            .to_string();
            let metadata = lookup
                .metadata
                .map(|bytes| String::from_utf8(bytes).expect("should be UTF-8"))
                .unwrap_or_else(|| "{}".to_owned());
            scenario
                .store
                .insert(
                    EC_ID,
                    trusted_server_core::ec::kv_backend::EcKvWrite {
                        body: &body,
                        metadata: &metadata,
                        ttl: std::time::Duration::from_secs(3_600),
                        mode: trusted_server_core::ec::kv_backend::EcKvWriteMode::Overwrite,
                    },
                )
                .expect("should store the version 1 row");
            scenario.store.reset_counts();
        }

        fn stored_body(scenario: &Scenario) -> serde_json::Value {
            let lookup = scenario
                .store
                .lookup(EC_ID)
                .expect("should read the row")
                .expect("should hold the row");
            serde_json::from_slice(&lookup.body).expect("should parse the stored row")
        }

        #[test]
        fn version_1_row_is_served_and_upgraded_by_the_next_write() {
            let scenario = auction_scenario();
            seed_version_1_row(&scenario);

            scenario.drive(auction_request(Some(&ec_cookie()), &auction_body(None)));

            assert_eq!(
                bidder_eids(&bidder_body(&scenario)),
                serde_json::json!([{
                    "source": "id5-sync.com",
                    "uids": [{
                        "id": "version-1-uid",
                        "atype": trusted_server_core::settings::EcPartner::default_openrtb_atype()
                    }]
                }]),
                "should serve the version 1 record as one entry with one UID"
            );
            assert_no_writes(&scenario.store, "/auction");
            assert_eq!(
                stored_body(&scenario)["v"],
                1,
                "control: the auction should leave the row at version 1"
            );

            let (_, outcome) = scenario.drive(navigation(Some(&returning_with_shared_id())));

            assert_eq!(
                outcome,
                Some(FlushOutcome::Written),
                "should flush the sharedId"
            );
            assert_writes(&scenario.store, (0, 1, 0), "one CAS write");
            let written = stored_body(&scenario);
            assert_eq!(written["v"], 2, "should write version 2");
            let record = &written["ids"]["id5-sync.com"];
            assert!(
                record.get("uid").is_none(),
                "should drop the version 1 uid field: {record}"
            );
            assert_eq!(
                record["eids"][0]["uids"][0]["id"], "version-1-uid",
                "should keep the UID as an entry"
            );
        }

        #[test]
        fn generic_pull_uid_written_after_send_is_served_on_the_next_auction() {
            let scenario = full_scenario();
            scenario.seed_live();

            let (_, outcome) = scenario.drive(navigation(Some(&ec_cookie())));

            assert_eq!(
                outcome,
                Some(FlushOutcome::Written),
                "should flush the pull"
            );
            let record = scenario.row().ids.get("ssp.example.com").cloned();
            assert_eq!(
                record.as_ref().and_then(|record| record.w.as_deref()),
                Some(PULL_PROVIDER),
                "should record the pull provider as the writer"
            );

            let (response, _) =
                scenario.drive(auction_request(Some(&ec_cookie()), &auction_body(None)));

            let atype = trusted_server_core::settings::EcPartner::default_openrtb_atype();
            let served = serde_json::json!([{
                "source": "ssp.example.com",
                "uids": [{ "id": "pull-uid-1", "atype": atype }]
            }]);
            assert_eq!(
                bidder_eids(&bidder_body(&scenario)),
                served,
                "should serve the pulled UID to bidders"
            );
            assert_eq!(
                mirror_of(&json_of(response)),
                serde_json::json!({ "v": 1, "eids": served }),
                "should mirror the pulled UID"
            );
        }

        /// As [`drive_with_runtime`], running `after_send` between the send
        /// and phase 4, where another request can land.
        fn drive_with_send_hook(
            state: &Arc<AppState>,
            request: HttpRequest,
            after_send: &mut dyn FnMut(),
        ) -> Option<FlushOutcome> {
            let router = TrustedServerApp::routes_for_state(state);
            let mut response = futures::executor::block_on(router.oneshot(request))
                .expect("should route the request");
            let mut ec_state = response
                .extensions_mut()
                .remove::<EcFinalizeState>()
                .expect("should carry EC state");
            let graph = || state.identity_graph.clone();
            finalize_send_and_flush(
                PostRouteInput {
                    runtime: Some(&state.identity),
                    settings: &state.settings,
                    ec_state: &mut ec_state,
                    graph: &graph,
                    rate_limiter: &AllowAll,
                    timings: None,
                },
                response,
                &mut |_| after_send(),
            )
        }

        /// Withdraws consent on an `/auction` call, as a second tab can.
        fn withdraw_on_auction(state: &Arc<AppState>) {
            let (response, _) = drive_with_runtime(
                state,
                auction_request(Some(&ec_cookie()), &auction_body(None)),
                Some(&state.identity),
                withdraw,
            );
            assert_no_server_identity_cookie(&response);
            assert!(
                set_cookies(&response)
                    .iter()
                    .any(|cookie| cookie.starts_with("ts-ec=;") && cookie.contains("Max-Age=0")),
                "control: the withdrawal should expire ts-ec: {:?}",
                set_cookies(&response)
            );
        }

        #[test]
        fn withdrawal_on_auction_after_navigation_send_makes_phase_four_drop_and_call_nothing() {
            let control = full_scenario();
            control.seed_live();
            let (_, outcome) = control.drive(navigation(Some(&returning_with_shared_id())));
            assert_eq!(
                outcome,
                Some(FlushOutcome::Written),
                "control: should flush"
            );
            assert_eq!(
                control.network.requests_to(PULL_HOST).len(),
                1,
                "control: should call the due provider"
            );

            let scenario = full_scenario();
            scenario.seed_live();
            let state = Arc::clone(&scenario.state);

            let outcome = drive_with_send_hook(
                &state,
                navigation(Some(&returning_with_shared_id())),
                &mut || withdraw_on_auction(&state),
            );

            assert_eq!(
                outcome,
                Some(FlushOutcome::Dropped("tombstone")),
                "should drop the navigation's mutation"
            );
            assert!(
                scenario.network.requests_to(PULL_HOST).is_empty(),
                "should call no provider after the tombstone"
            );
            assert_writes(
                &scenario.store,
                (1, 1, 0),
                "the withdrawal's marker and tombstone only, nothing from phase 4",
            );
            let row = scenario.row();
            assert!(!row.consent.ok, "should leave the tombstone");
            assert!(row.ids.is_empty(), "should hold no ID after the tombstone");
        }

        #[test]
        fn withdrawal_returns_clear_for_the_mirror() {
            let scenario = auction_scenario();
            scenario.seed(&row_with_set(Some(FAR_FUTURE)));
            let (before, _) =
                scenario.drive(auction_request(Some(&ec_cookie()), &auction_body(None)));
            assert_eq!(
                mirror_of(&json_of(before)),
                serde_json::json!({ "v": 1, "eids": full_entry_set_eids() }),
                "control: a live row should be mirrored"
            );

            withdraw_on_auction(&scenario.state);
            let (after, _) =
                scenario.drive(auction_request(Some(&ec_cookie()), &auction_body(None)));

            assert_eq!(
                mirror_of(&json_of(after)),
                serde_json::json!({ "clear": true }),
                "should tell the shim to expire the mirror"
            );
            assert_eq!(
                bidder_eids(&bidder_body(&scenario)),
                serde_json::Value::Null,
                "should send bidders no ID from a tombstoned row"
            );
        }

        #[test]
        fn consent_withdrawal_writes_tombstone_and_marker() {
            // (refused conditional writes, tombstone CAS attempts, marker
            // writes, whether the row ends tombstoned)
            let cases = [
                (0_usize, 1_usize, 1_usize, true),
                (5, 6, 1, true),
                (10, 10, 0, false),
            ];
            for (refused, tombstone_attempts, markers, tombstoned) in cases {
                let scenario = Scenario::new();
                scenario.seed_live();
                if refused > 0 {
                    scenario.store.refuse_conditional_writes(EC_ID, refused);
                }

                let (response, outcome) = drive_with_runtime(
                    &scenario.state,
                    navigation(Some(&ec_cookie())),
                    Some(&scenario.state.identity),
                    withdraw,
                );

                assert_no_server_identity_cookie(&response);
                assert_eq!(
                    outcome,
                    Some(FlushOutcome::Skipped),
                    "should flush nothing after a withdrawal: {refused} refused"
                );
                assert_writes(
                    &scenario.store,
                    (markers, tombstone_attempts, 0),
                    &format!("{refused} refused writes: the marker, then every tombstone attempt"),
                );
                assert_eq!(
                    !scenario.row().consent.ok,
                    tombstoned,
                    "should end tombstoned only when an attempt landed: {refused} refused"
                );
            }
        }

        #[test]
        fn cas_conflict_on_flush_is_one_attempt() {
            let scenario = Scenario::new();
            scenario.seed_live();
            scenario.store.refuse_conditional_writes(EC_ID, 5);

            let (_, outcome) = scenario.drive(navigation(Some(&returning_with_shared_id())));

            assert_eq!(
                outcome,
                Some(FlushOutcome::WriteFailed {
                    removals: trusted_server_core::ec::identity::flush::RemovalWriteOutcome::NothingToWrite
                }),
                "should fail the flush with no removal to write"
            );
            assert_writes(
                &scenario.store,
                (0, 1, 0),
                "one refused attempt, and no retry although four more would be refused",
            );

            let control = Scenario::new();
            control.seed_live();
            control.drive(navigation(Some(&returning_with_shared_id())));
            assert_writes(&control.store, (0, 1, 0), "control: one attempt that lands");
        }

        #[test]
        fn refused_flush_is_rederived_by_the_next_navigation() {
            let scenario = Scenario::new();
            scenario.seed_live();
            scenario.store.refuse_conditional_writes(EC_ID, 1);

            scenario.drive(navigation(Some(&returning_with_shared_id())));

            assert_eq!(
                stored_uid(&scenario.row(), "sharedid.org"),
                None,
                "should not hold the ID after the refused write"
            );
            assert_writes(&scenario.store, (0, 1, 0), "the refused attempt");

            let (_, outcome) = scenario.drive(navigation(Some(&returning_with_shared_id())));

            assert_eq!(
                outcome,
                Some(FlushOutcome::Written),
                "should write the second time"
            );
            assert_writes(&scenario.store, (0, 2, 0), "one attempt per navigation");
            assert_eq!(
                stored_uid(&scenario.row(), "sharedid.org").as_deref(),
                Some(SHARED_ID),
                "should hold the ingest the second navigation re-derived from the same cookies"
            );
        }

        #[test]
        fn deleted_row_is_found_by_the_next_writer_and_rotated() {
            let scenario = Scenario::new();
            scenario.seed_live();
            scenario
                .graph()
                .delete(EC_ID)
                .expect("should delete the row out of band");
            scenario.store.reset_counts();

            let (response, _) = scenario.drive(navigation(Some(&returning_with_shared_id())));

            let rotated = set_cookie_value(&response, "ts-ec").expect("should set the new EC");
            assert_ne!(rotated, EC_ID, "should rotate to a new ID");
            assert_writes(
                &scenario.store,
                (1, 0, 0),
                "one Add creates the replacement",
            );
            assert_eq!(
                stored_uid(&scenario.row_of(&rotated), "sharedid.org").as_deref(),
                Some(SHARED_ID),
                "should carry the ingest in the replacement row"
            );
            assert!(
                scenario.graph().get(EC_ID).expect("should read").is_none(),
                "should not resurrect the deleted ID"
            );
        }

        #[test]
        fn stager_that_sees_missing_row_returns_clear_and_next_writer_recovers() {
            let live = auction_scenario();
            live.seed(&row_with_set(Some(FAR_FUTURE)));
            let (served, _) = live.drive(auction_request(Some(&ec_cookie()), &auction_body(None)));
            assert_ne!(
                mirror_of(&json_of(served)),
                serde_json::json!({ "clear": true }),
                "control: a live row should be mirrored"
            );

            let scenario = auction_scenario();
            let (response, _) =
                scenario.drive(auction_request(Some(&ec_cookie()), &auction_body(None)));

            assert_eq!(
                mirror_of(&json_of(response)),
                serde_json::json!({ "clear": true }),
                "should return clear for a missing row"
            );
            assert_no_writes(&scenario.store, "the stager stores nothing");
            assert!(
                scenario.graph().get(EC_ID).expect("should read").is_none(),
                "should not create the row"
            );

            let (navigated, _) = scenario.drive(navigation(Some(&returning_with_shared_id())));

            let rotated = set_cookie_value(&navigated, "ts-ec").expect("should set the new EC");
            assert_ne!(rotated, EC_ID, "the next writer should recover by rotating");
            assert_writes(&scenario.store, (1, 0, 0), "one Add by the writer");
        }

        const REGISTERED_ONLY_COOKIE: &str =
            include_str!("../../trusted-server-js/lib/test/fixtures/ts-eids-registered-only.txt");
        const ORTB2_ONLY_COOKIE: &str =
            include_str!("../../trusted-server-js/lib/test/fixtures/ts-eids-ortb2-only.txt");

        /// A provider that answers every call by invalidating the ID of its
        /// one source, so the flush carries a removal.
        struct InvalidatingProvider {
            descriptor: trusted_server_core::ec::identity::ProviderDescriptor,
            source: trusted_server_core::ec::identity::SourceDomain,
        }

        impl InvalidatingProvider {
            fn new(id: &str, source: &str) -> Self {
                use trusted_server_core::ec::identity::{ProviderDescriptor, Resolves};
                let source: trusted_server_core::ec::identity::SourceDomain =
                    source.parse().expect("should parse the source");
                let descriptor =
                    ProviderDescriptor::builder(id.parse().expect("should parse the provider ID"))
                        .with_source(source.clone())
                        .with_resolves(
                            Resolves::new().with_endpoint("https://probe.example.com/resolve"),
                        )
                        .with_consent_policy(
                            trusted_server_core::ec::identity::test_support::resolver_policy(),
                        )
                        .build()
                        .expect("should build the descriptor");
                Self { descriptor, source }
            }
        }

        impl trusted_server_core::ec::identity::IdentityProvider for InvalidatingProvider {
            fn descriptor(&self) -> &trusted_server_core::ec::identity::ProviderDescriptor {
                &self.descriptor
            }

            fn request(
                &self,
                input: &trusted_server_core::ec::identity::ResolveInput<'_>,
            ) -> Result<
                Option<trusted_server_core::ec::identity::ProviderRequest>,
                Report<trusted_server_core::ec::identity::IdentityProviderError>,
            > {
                Ok(Some(
                    trusted_server_core::ec::identity::ProviderRequest::get(input.endpoint()?),
                ))
            }

            fn response(
                &self,
                response: &trusted_server_core::ec::identity::ProviderResponse,
                _input: &trusted_server_core::ec::identity::ResolveInput<'_>,
            ) -> Result<
                trusted_server_core::ec::identity::Resolution,
                Report<trusted_server_core::ec::identity::IdentityProviderError>,
            > {
                response.success_text()?;
                Ok(trusted_server_core::ec::identity::Resolution::invalidate(
                    &self.source,
                ))
            }
        }

        /// A scenario whose one provider invalidates the ID it wrote for
        /// `ssp.example.com`, which the seeded row holds and is due to refresh.
        fn invalidating_scenario() -> Scenario {
            let Scenario {
                state,
                store,
                network,
            } = Scenario::new();
            let provider: Arc<dyn trusted_server_core::ec::identity::IdentityProvider> = Arc::new(
                InvalidatingProvider::new("removal-probe", "ssp.example.com"),
            );
            let runtime = IdentityRuntime::from_settings_with_providers(
                &state.settings,
                vec![provider],
                BTreeMap::from([(
                    "ssp.example.com".parse().expect("should parse the source"),
                    "removal-probe"
                        .parse()
                        .expect("should parse the provider ID"),
                )]),
            )
            .expect("should build the identity runtime");
            let scenario = Scenario {
                state: crate::app::app_state_with_identity_for_test(state, runtime),
                store,
                network,
            };
            scenario
                .network
                .respond("probe.example.com", 200, "text/plain", "ok");
            let mut entry = live_entry();
            entry.ids.insert(
                "ssp.example.com".to_owned(),
                KvPartnerId {
                    w: Some("removal-probe".to_owned()),
                    rv: Some("0123456789abcdef".to_owned()),
                    at: Some(1_000),
                    ra: Some(1_500),
                    ..KvPartnerId::new("probe-uid")
                },
            );
            scenario.seed(&entry);
            scenario
        }

        #[test]
        fn refused_flush_writes_its_removals_alone() {
            let control = invalidating_scenario();
            control.drive(navigation(Some(&returning_with_shared_id())));
            assert_writes(&control.store, (0, 1, 0), "control: one write holds both");
            let row = control.row();
            assert_eq!(
                stored_uid(&row, "ssp.example.com"),
                None,
                "control: the removal should be in the one write"
            );
            assert_eq!(
                stored_uid(&row, "sharedid.org").as_deref(),
                Some(SHARED_ID),
                "control: so should the ingest"
            );

            let refused_once = invalidating_scenario();
            refused_once.store.refuse_conditional_writes(EC_ID, 1);

            let (_, outcome) = refused_once.drive(navigation(Some(&returning_with_shared_id())));

            assert_eq!(
                outcome,
                Some(FlushOutcome::WriteFailed {
                    removals:
                        trusted_server_core::ec::identity::flush::RemovalWriteOutcome::Written
                }),
                "should write the removals after the flush is refused"
            );
            assert_writes(
                &refused_once.store,
                (0, 2, 0),
                "the refused flush, then the removals alone",
            );
            let row = refused_once.row();
            assert_eq!(
                stored_uid(&row, "ssp.example.com"),
                None,
                "should have removed the invalidated ID"
            );
            assert_eq!(
                stored_uid(&row, "sharedid.org"),
                None,
                "should not write the refused ingest with the removals"
            );

            let refused_always = invalidating_scenario();
            refused_always.store.refuse_conditional_writes(EC_ID, 6);

            let (_, outcome) = refused_always.drive(navigation(Some(&returning_with_shared_id())));

            assert_eq!(
                outcome,
                Some(FlushOutcome::WriteFailed {
                    removals: trusted_server_core::ec::identity::flush::RemovalWriteOutcome::Failed
                }),
                "should report the removal write failed when every attempt is refused"
            );
            assert_writes(
                &refused_always.store,
                (1, 6, 0),
                "the flush once, the removals up to five times, then the pending key",
            );
            assert_eq!(
                stored_uid(&refused_always.row(), "ssp.example.com").as_deref(),
                Some("probe-uid"),
                "should leave the row as it was"
            );

            let (_, next) = refused_always.drive(navigation(Some(&format!("ts-ec={EC_ID}"))));

            assert_eq!(
                next,
                Some(FlushOutcome::Written),
                "should merge the pending key on the next writer"
            );
            assert_eq!(
                stored_uid(&refused_always.row(), "ssp.example.com"),
                None,
                "should apply the removal the earlier flush missed"
            );
        }

        /// A scenario whose Didomi proxy answers `body`, with a capture-only
        /// provider listening on that proxy's path and a row holding a
        /// browser-written `ssp.example.com` ID for it to invalidate.
        fn capture_scenario(body: &str) -> Scenario {
            capture_scenario_with(body, false)
        }

        /// As [`capture_scenario`], with the provider's `revoke_scope` set to
        /// `"global"` when `global`. The settings are changed after the app
        /// state is built, as config validation knows no capture-only
        /// provider.
        fn capture_scenario_with(body: &str, global: bool) -> Scenario {
            let Scenario {
                mut state,
                store,
                network,
            } = Scenario::with_settings(DIDOMI_TOML);
            if global {
                let owned = Arc::get_mut(&mut state).expect("should own the fresh state");
                Arc::make_mut(&mut owned.settings)
                    .ec
                    .identity
                    .providers
                    .insert(
                        "didomi".to_owned(),
                        trusted_server_core::settings::ProviderSettings {
                            revoke_scope: Some(trusted_server_core::settings::RevokeScope::Global),
                            ..trusted_server_core::settings::ProviderSettings::default()
                        },
                    );
            }
            let provider =
                trusted_server_core::ec::identity::test_support::answering_capture_provider(
                    "didomi",
                    &["ssp.example.com"],
                    None,
                );
            let runtime = IdentityRuntime::from_settings_with_providers(
                &state.settings,
                vec![provider],
                BTreeMap::new(),
            )
            .expect("should build the identity runtime");
            let scenario = Scenario {
                state: crate::app::app_state_with_identity_for_test(state, runtime),
                store,
                network,
            };
            scenario
                .network
                .respond("sdk.example.com", 200, "application/json", body);
            let mut entry = live_entry();
            entry.ids.insert(
                "ssp.example.com".to_owned(),
                KvPartnerId {
                    rv: Some("0123456789abcdef".to_owned()),
                    at: Some(1_000),
                    ..KvPartnerId::new("probe-uid")
                },
            );
            scenario.seed(&entry);
            scenario
        }

        const CAPTURE_INVALIDATION: &str = r#"{"invalidate":["ssp.example.com"]}"#;

        #[test]
        fn capture_removal_is_a_separate_write_beside_the_one_flush() {
            let control = capture_scenario(r#"{}"#);
            control.drive(didomi_navigation(Some(&returning_with_shared_id())));
            assert_writes(
                &control.store,
                (0, 1, 0),
                "control: a proxied navigation with nothing to capture flushes once",
            );

            let scenario = capture_scenario(CAPTURE_INVALIDATION);

            let (response, outcome) =
                scenario.drive(didomi_navigation(Some(&returning_with_shared_id())));

            assert_eq!(response.status(), 200, "should serve the proxied answer");
            assert_eq!(
                outcome,
                Some(FlushOutcome::Written),
                "should still flush the writer's ingest"
            );
            assert_writes(
                &scenario.store,
                (0, 2, 0),
                "the capture removal write, then the writer's one flush",
            );
            let row = scenario.row();
            assert_eq!(
                stored_uid(&row, "ssp.example.com"),
                None,
                "should have removed the captured invalidation"
            );
            assert_eq!(
                stored_uid(&row, "sharedid.org").as_deref(),
                Some(SHARED_ID),
                "should keep the writer's own ingest in its flush"
            );
        }

        #[test]
        fn capture_global_revoke_tombstones_the_row_and_expires_the_ec_cookie() {
            let scenario = capture_scenario_with(r#"{"opted_out":true}"#, true);

            let (response, _) =
                scenario.drive(didomi_navigation(Some(&returning_with_shared_id())));

            assert_eq!(response.status(), 200, "should serve the proxied answer");
            let row = scenario.row();
            assert!(!row.consent.ok, "should tombstone the row");
            assert!(row.ids.is_empty(), "should remove every ID");
            assert!(
                set_cookies(&response).iter().any(|cookie| {
                    cookie_name(cookie) == "ts-ec" && cookie.contains("Max-Age=0")
                }),
                "should expire the EC cookie"
            );
            let writes = scenario.store.writes();
            assert_eq!(
                (writes.if_generation_match, writes.overwrite),
                (1, 0),
                "should write one tombstone and no flush"
            );
        }

        #[test]
        fn failed_capture_removal_write_runs_once_more_after_send() {
            let scenario = capture_scenario(CAPTURE_INVALIDATION);
            scenario.store.refuse_conditional_writes(EC_ID, 5);

            let (outcome, measurement) = drive_measured_for_test(
                &scenario.state,
                didomi_navigation(Some(&returning_with_shared_id())),
            );

            assert_eq!(
                outcome,
                Some(FlushOutcome::Written),
                "should flush the ingest too"
            );
            assert_writes(
                &scenario.store,
                (0, 7, 0),
                "five refused attempts before send, the retry after send, the flush",
            );
            assert_eq!(
                stored_uid(&scenario.row(), "ssp.example.com"),
                None,
                "should have written the removal after send"
            );
            assert_eq!(measurement.removal_lost(), 0, "should lose no removal");
        }

        #[test]
        fn capture_removal_after_both_attempts_reaches_the_row_through_the_pending_key() {
            let scenario = capture_scenario(CAPTURE_INVALIDATION);
            scenario.store.refuse_conditional_writes(EC_ID, 10);

            let (outcome, measurement) = drive_measured_for_test(
                &scenario.state,
                didomi_navigation(Some(&returning_with_shared_id())),
            );

            assert_eq!(
                outcome,
                Some(FlushOutcome::Written),
                "should flush the merge"
            );
            assert_eq!(measurement.removal_lost(), 0, "should lose no removal");
            assert!(
                measurement.to_string().contains("removal_deferred=1"),
                "should print the deferral on the measurement line"
            );
            assert_eq!(
                stored_uid(&scenario.row(), "ssp.example.com"),
                None,
                "should apply the removal in the writer's flush"
            );
        }

        #[test]
        fn capture_removal_lost_after_both_attempts_is_counted() {
            let scenario = capture_scenario(CAPTURE_INVALIDATION);
            scenario.store.refuse_conditional_writes(EC_ID, 10);
            scenario.store.fail_all_writes(&format!("pending:{EC_ID}"));

            let (_, measurement) = drive_measured_for_test(
                &scenario.state,
                didomi_navigation(Some(&returning_with_shared_id())),
            );

            assert_eq!(
                measurement.removal_lost(),
                1,
                "should count the removal lost after both attempts"
            );
            assert!(
                measurement.to_string().contains("removal_lost=1"),
                "should print the count on the measurement line"
            );
        }

        #[test]
        fn capture_positive_ids_are_counted_on_the_measurement_line_and_not_stored() {
            let scenario = capture_scenario(
                r#"{"tokens":[{"source":"ssp.example.com","value":"captured-secret-uid"}]}"#,
            );

            let (_, measurement) = drive_measured_for_test(
                &scenario.state,
                didomi_navigation(Some(&returning_with_shared_id())),
            );

            let line = measurement.to_string();
            assert!(
                line.contains("capture_unpersisted=didomi/ssp.example.com:1"),
                "should count the positive ID: {line}"
            );
            assert!(
                !line.contains("captured-secret-uid"),
                "should print no value: {line}"
            );
            assert_eq!(
                stored_uid(&scenario.row(), "ssp.example.com").as_deref(),
                Some("probe-uid"),
                "should not store the captured ID"
            );
        }

        #[test]
        fn registered_source_beyond_todays_trim_reaches_kv_on_next_navigation() {
            use base64::Engine as _;
            let cookie_value = REGISTERED_ONLY_COOKIE.trim_end();
            let decoded: serde_json::Value = serde_json::from_slice(
                &base64::engine::general_purpose::STANDARD
                    .decode(cookie_value)
                    .expect("should decode the shim's cookie"),
            )
            .expect("should parse the shim's cookie");
            let sources: Vec<&str> = decoded
                .as_array()
                .expect("should be a list")
                .iter()
                .filter_map(|entry| entry["source"].as_str())
                .collect();
            assert_eq!(
                sources,
                ["registered.example.com"],
                "control: the shim should keep only the registered source"
            );
            let scenario = Scenario::with_settings(&partner_toml("registered.example.com"));
            scenario.seed_live();

            let (_, outcome) = scenario.drive(navigation(Some(&format!(
                "ts-ec={EC_ID}; ts-eids={cookie_value}"
            ))));

            assert_eq!(outcome, Some(FlushOutcome::Written), "should flush");
            assert_writes(&scenario.store, (0, 1, 0), "one write");
            let row = scenario.row();
            assert_eq!(
                stored_uid(&row, "registered.example.com"),
                Some("x".repeat(400)),
                "should hold the registered source the old trim would have dropped"
            );
            assert!(
                (0..7).all(|index| !row.ids.contains_key(&format!("u{index}.example.com"))),
                "should hold no unregistered source: {:?}",
                row.ids.keys().collect::<Vec<_>>()
            );
        }

        #[test]
        fn ortb2_only_eid_reaches_kv() {
            let scenario = Scenario::new();
            scenario.seed_live();
            assert_eq!(
                stored_uid(&scenario.row(), "id5-sync.com"),
                None,
                "control: id5-sync.com should be absent from KV before the navigation"
            );
            let cookie = format!("ts-ec={EC_ID}; ts-eids={}", ORTB2_ONLY_COOKIE.trim_end());

            let (_, outcome) = scenario.drive(navigation(Some(&cookie)));

            assert_eq!(outcome, Some(FlushOutcome::Written), "should flush");
            assert_writes(&scenario.store, (0, 1, 0), "one write");
            assert_eq!(
                stored_uid(&scenario.row(), "id5-sync.com").as_deref(),
                Some("fictional-id5"),
                "should hold the ID5 UID the shim copied from the ortb2 config"
            );
        }

        /// Everything a visitor's navigation, `/auction` and `/identify`
        /// produce, for comparing runs.
        #[derive(Debug, PartialEq)]
        struct Observed {
            navigation: (u16, Option<FlushOutcome>),
            origin_cookie: Option<String>,
            row_uids: BTreeMap<String, Option<String>>,
            bidder_request: serde_json::Value,
            auction_header_eids: serde_json::Value,
            auction_mirror: serde_json::Value,
            identify: (u16, serde_json::Value),
            writes: (usize, usize, usize),
        }

        fn observe_with_mirror_cookie(mirror: Option<&str>) -> Observed {
            let scenario = auction_scenario();
            let mut entry = live_entry();
            entry.ids.insert(
                "id5-sync.com".to_owned(),
                KvPartnerId::browser("server-id5", Some(1_500)),
            );
            scenario.seed(&entry);
            let mut cookie = returning_with_shared_id();
            if let Some(mirror) = mirror {
                cookie.push_str(&format!("; __Host-ts-eids-s={mirror}"));
            }

            let (nav, nav_outcome) = scenario.drive(navigation(Some(&cookie)));
            let (auction, _) = scenario.drive(auction_request(Some(&cookie), &auction_body(None)));
            let (identify, _) = scenario.drive(identify_request(&cookie, "sharedid.org"));

            let mut bidder_request = bidder_body(&scenario);
            bidder_request
                .as_object_mut()
                .expect("should be an object")
                .remove("id");
            let row = scenario.row();
            let writes = scenario.store.writes();
            let origin_cookie = scenario
                .network
                .requests_to(ORIGIN_HOST)
                .first()
                .and_then(|seen| seen.headers.get("cookie"))
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            Observed {
                navigation: (nav.status().as_u16(), nav_outcome),
                origin_cookie,
                row_uids: row
                    .ids
                    .keys()
                    .map(|source| (source.clone(), stored_uid(&row, source)))
                    .collect(),
                bidder_request,
                auction_header_eids: header_eids(&auction),
                auction_mirror: mirror_of(&json_of(auction)),
                identify: (identify.status().as_u16(), json_of(identify)),
                writes: (writes.add, writes.if_generation_match, writes.overwrite),
            }
        }

        #[test]
        fn forged_mirror_cookie_changes_nothing() {
            use base64::Engine as _;
            let encode = |value: &serde_json::Value| {
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(serde_json::to_vec(value).expect("should serialize the mirror"))
            };
            let forged = encode(&serde_json::json!({
                "v": 1,
                "eids": [
                    { "source": "sharedid.org", "uids": [{ "id": "forged-uid", "atype": 1 }] },
                    { "source": "forged.example.com", "uids": [{ "id": "forged-uid", "atype": 1 }] }
                ]
            }));
            let stale = encode(&serde_json::json!({
                "v": 1,
                "eids": [{ "source": "id5-sync.com", "uids": [{ "id": "stale-id5", "atype": 1 }] }]
            }));
            let baseline = observe_with_mirror_cookie(None);
            let shown = format!("{baseline:?}");
            assert!(
                shown.contains("server-id5") && shown.contains(SHARED_ID),
                "control: the baseline should hold the server and browser IDs: {shown}"
            );
            assert_eq!(
                baseline.writes,
                (0, 1, 0),
                "control: the navigation should write once"
            );

            for (name, value) in [
                ("forged", forged.as_str()),
                ("stale", stale.as_str()),
                ("malformed", "%%%not-base64"),
                ("empty", ""),
            ] {
                let observed = observe_with_mirror_cookie(Some(value));

                assert_eq!(
                    observed, baseline,
                    "should change no bidder request, answer or write: {name}"
                );
                let shown = format!("{observed:?}");
                assert!(
                    !shown.contains("forged-uid") && !shown.contains("stale-id5"),
                    "should carry nothing from the {name} mirror: {shown}"
                );
            }
        }

        /// What the code before the identity work, run on this input,
        /// stores: the first UID of the last entry of each registered
        /// source, with the sharedId cookie last, and nothing for an
        /// unregistered source.
        const GOLDEN_INGEST_COOKIE: &str = "W3siZXh0Ijp7InNlZ21lbnRzIjpbInMxIl19LCJpbnNlcnRlciI6Imluc2VydGVyLmV4YW1wbGUuY29tIiwibW0iOjIsInNvdXJjZSI6ImlkNS1zeW5jLmNvbSIsInVpZHMiOlt7ImF0eXBlIjoxLCJpZCI6ImlkNS1maXJzdCJ9LHsiYXR5cGUiOjEsImlkIjoiaWQ1LXNlY29uZCJ9XX0seyJzb3VyY2UiOiJ1bnJlZ2lzdGVyZWQuZXhhbXBsZS5jb20iLCJ1aWRzIjpbeyJhdHlwZSI6MSwiaWQiOiJ1bnJlZy0xIn1dfSx7InNvdXJjZSI6InNoYXJlZGlkLm9yZyIsInVpZHMiOlt7ImF0eXBlIjoxLCJpZCI6InByZWJpZC1zaGFyZWQifV19LHsiaW5zZXJ0ZXIiOiJsYXRlci5leGFtcGxlLmNvbSIsIm1tIjoxLCJzb3VyY2UiOiJpZDUtc3luYy5jb20iLCJ1aWRzIjpbeyJhdHlwZSI6MSwiaWQiOiJpZDUtbGFzdC1maXJzdCJ9LHsiYXR5cGUiOjEsImlkIjoiaWQ1LWxhc3Qtc2Vjb25kIn1dfV0=";

        /// The `x-ts-eids` value the code before the identity work builds
        /// for [`GOLDEN_ROW_EIDS`].
        const GOLDEN_ROW_HEADER: &str = "W3sic291cmNlIjoiaWQ1LXN5bmMuY29tIiwidWlkcyI6W3siaWQiOiJpZDUtdWlkIiwiYXR5cGUiOjN9XX0seyJzb3VyY2UiOiJzaGFyZWRpZC5vcmciLCJ1aWRzIjpbeyJpZCI6InNoYXJlZC11aWQiLCJhdHlwZSI6M31dfV0=";

        /// A body with repeated sources and EID-level fields.
        const GOLDEN_BODY_EIDS: &str = r#"[
            {"inserter":"adserver.org","matcher":"adserver.org","mm":4,"source":"adserver.org","uids":[{"atype":1,"ext":{"rtiPartner":"TDID"},"id":"body-tdid"}]},
            {"ext":{"segments":["s1"]},"inserter":"other.example.com","mm":1,"source":"adserver.org","uids":[{"atype":1,"id":"body-tdid-2"}]},
            {"source":"ids.example.com","uids":[{"atype":3,"id":"ids-uid"}]}
        ]"#;

        /// What the code before the identity work forwards for
        /// [`GOLDEN_BODY_EIDS`] and an empty row.
        const GOLDEN_BODY_FORWARDED: &str = r#"[
            {"source":"adserver.org","uids":[{"id":"body-tdid","atype":1,"ext":{"rtiPartner":"TDID"}}],"inserter":"adserver.org","matcher":"adserver.org","mm":4},
            {"source":"adserver.org","uids":[{"id":"body-tdid-2","atype":1}],"inserter":"other.example.com","mm":1,"ext":{"segments":["s1"]}},
            {"source":"ids.example.com","uids":[{"id":"ids-uid","atype":3}]}
        ]"#;

        fn golden_expected_ingest() -> BTreeMap<String, String> {
            BTreeMap::from([
                ("id5-sync.com".to_owned(), "id5-last-first".to_owned()),
                ("sharedid.org".to_owned(), "shared-1".to_owned()),
            ])
        }

        /// Each source's first UID, with a check that the record is one
        /// entry holding one UID and nothing else.
        fn assert_one_uid_records(row: &KvEntry) -> BTreeMap<String, String> {
            row.ids
                .iter()
                .map(|(source, record)| {
                    assert_eq!(record.w, None, "should write no provider: {source}");
                    assert_eq!(record.eids.len(), 1, "should hold one entry: {source}");
                    let entry = &record.eids[0];
                    assert_eq!(entry.uids.len(), 1, "should hold one UID: {source}");
                    assert_eq!(
                        (&entry.inserter, &entry.matcher, entry.mm, &entry.ext),
                        (&None, &None, None, &None),
                        "should hold no EID-level field: {source}"
                    );
                    assert_eq!(
                        (entry.uids[0].atype, &entry.uids[0].ext),
                        (None, &None),
                        "should hold no UID-level field: {source}"
                    );
                    (source.clone(), entry.uids[0].id.clone())
                })
                .collect()
        }

        #[test]
        fn golden_browser_ingest_writes_the_first_uid() {
            let cookie = format!("ts-eids={GOLDEN_INGEST_COOKIE}; sharedId=shared-1");

            let returning = Scenario::new();
            returning.seed_live();
            let (_, outcome) =
                returning.drive(navigation(Some(&format!("ts-ec={EC_ID}; {cookie}"))));
            assert_eq!(outcome, Some(FlushOutcome::Written), "should flush");
            assert_writes(&returning.store, (0, 1, 0), "one flush");
            assert_eq!(
                assert_one_uid_records(&returning.row()),
                golden_expected_ingest(),
                "the flushed row should hold the UIDs the code before this spec stored"
            );

            let new_visitor = Scenario::new();
            let (response, _) = new_visitor.drive(navigation(Some(&cookie)));
            let minted = set_cookie_value(&response, "ts-ec").expect("should mint an EC");
            assert_writes(&new_visitor.store, (1, 0, 0), "one create");
            assert_eq!(
                assert_one_uid_records(&new_visitor.row_of(&minted)),
                golden_expected_ingest(),
                "the created row should hold the same UIDs"
            );
        }

        #[test]
        fn golden_one_uid_row_goes_out_unchanged() {
            let scenario = auction_scenario();
            let mut entry = live_entry();
            for (source, uid) in [("id5-sync.com", "id5-uid"), ("sharedid.org", "shared-uid")] {
                entry
                    .ids
                    .insert(source.to_owned(), KvPartnerId::browser(uid, Some(1_500)));
            }
            scenario.seed(&entry);

            let (response, _) =
                scenario.drive(auction_request(Some(&ec_cookie()), &auction_body(None)));

            let expected: serde_json::Value = serde_json::json!([
                { "source": "id5-sync.com", "uids": [{ "id": "id5-uid", "atype": 3 }] },
                { "source": "sharedid.org", "uids": [{ "id": "shared-uid", "atype": 3 }] }
            ]);
            assert_eq!(
                bidder_eids(&bidder_body(&scenario)),
                expected,
                "user.ext.eids should equal what the code before this spec sent"
            );
            assert_eq!(
                response
                    .headers()
                    .get("x-ts-eids")
                    .and_then(|value| value.to_str().ok()),
                Some(GOLDEN_ROW_HEADER),
                "x-ts-eids should equal what the code before this spec built, byte for byte"
            );
        }

        #[test]
        fn golden_auction_body_forwarding_is_unchanged() {
            let scenario = auction_scenario();
            scenario.seed_live();
            let eids: serde_json::Value =
                serde_json::from_str(GOLDEN_BODY_EIDS).expect("should parse the body EIDs");
            let forwarded: serde_json::Value = serde_json::from_str(GOLDEN_BODY_FORWARDED)
                .expect("should parse the forwarded EIDs");
            assert!(
                forwarded.to_string().contains("rtiPartner")
                    && forwarded.to_string().contains("other.example.com"),
                "control: the golden should hold UID-level and EID-level fields"
            );

            scenario.drive(auction_request(
                Some(&ec_cookie()),
                &auction_body(Some(&eids)),
            ));

            assert_eq!(
                bidder_eids(&bidder_body(&scenario)),
                forwarded,
                "user.ext.eids should equal what the code before this spec forwarded"
            );
            assert_no_writes(&scenario.store, "/auction");
        }

        // ---- A registered provider's complete set, end to end (Task 39b) ----

        /// The provider `MultiEntryTestProvider` registers as.
        const MULTI_PROVIDER: &str = "exampleid";

        /// The host `MultiEntryTestProvider` calls.
        const MULTI_HOST: &str = "api.exampleid.example.com";

        /// The registration `exampleid` makes for `provider`.
        fn multi_registration(provider: MultiEntryTestProvider) -> IntegrationRegistration {
            IntegrationRegistration::builder("exampleid")
                .try_with_identity_provider(Ok::<_, Report<IdentityProviderError>>(provider))
                .expect("should register the provider")
                .build()
        }

        /// A scenario with the auction, the `exampleid` source registered for
        /// the bidstream, and `registrations` contributing providers.
        /// `exampleid` owns [`MULTI_ENTRY_SOURCE`] and runs in `mode`.
        fn registered_scenario(
            registrations: Vec<IntegrationRegistration>,
            mode: ProviderMode,
        ) -> Scenario {
            let Scenario {
                mut state,
                store,
                network,
            } = Scenario::with_settings(&format!(
                "{AUCTION_TOML}{}",
                partner_toml(MULTI_ENTRY_SOURCE)
            ));
            let owned = Arc::get_mut(&mut state).expect("should own the fresh state");
            Arc::make_mut(&mut owned.settings)
                .ec
                .identity
                .providers
                .insert(
                    MULTI_PROVIDER.to_owned(),
                    ProviderSettings {
                        mode,
                        ..ProviderSettings::default()
                    },
                );
            let providers = registrations
                .into_iter()
                .flat_map(|registration| registration.identity_providers)
                .collect();
            let runtime = IdentityRuntime::from_settings_with_providers(
                &state.settings,
                providers,
                BTreeMap::from([(
                    MULTI_ENTRY_SOURCE.parse().expect("should parse the source"),
                    MULTI_PROVIDER
                        .parse()
                        .expect("should parse the provider ID"),
                )]),
            )
            .expect("should build the identity runtime");
            let scenario = Scenario {
                state: crate::app::app_state_with_identity_for_test(state, runtime),
                store,
                network,
            };
            scenario.network.respond(
                BIDDER_HOST,
                200,
                "application/json",
                r#"{"id":"response","seatbid":[]}"#,
            );
            answer_with(&scenario, "full");
            scenario
        }

        /// A scenario whose one provider is `MultiEntryTestProvider`.
        fn multi_scenario(provider: MultiEntryTestProvider, mode: ProviderMode) -> Scenario {
            registered_scenario(vec![multi_registration(provider)], mode)
        }

        /// Makes the provider's endpoint answer with the `entries` shape.
        fn answer_with(scenario: &Scenario, entries: &str) {
            scenario.network.respond(
                MULTI_HOST,
                200,
                "application/json",
                &serde_json::json!({ "entries": entries }).to_string(),
            );
        }

        fn kv_uid(id: &str, atype: Option<i32>, ext: Option<serde_json::Value>) -> KvUid {
            KvUid {
                id: id.to_owned(),
                atype,
                ext,
            }
        }

        /// What the provider's `full` answer stores.
        fn multi_full_set() -> Vec<KvEid> {
            vec![
                KvEid {
                    uids: vec![
                        kv_uid(
                            "uid-a",
                            Some(1),
                            Some(serde_json::json!({ "rtiPartner": "example" })),
                        ),
                        kv_uid("uid-b", None, None),
                    ],
                    inserter: Some("inserter-a.example.com".to_owned()),
                    matcher: Some("matcher-a.example.com".to_owned()),
                    mm: Some(2),
                    ext: None,
                },
                KvEid {
                    uids: vec![kv_uid("uid-c", None, None)],
                    inserter: None,
                    matcher: None,
                    mm: None,
                    ext: Some(serde_json::json!({ "segments": ["example"] })),
                },
            ]
        }

        /// The first entry of the `full` answer, which the `first` answer
        /// returns alone.
        fn multi_first_set() -> Vec<KvEid> {
            multi_full_set().into_iter().take(1).collect()
        }

        /// The first entry as bidders see it.
        fn multi_first_eid() -> serde_json::Value {
            serde_json::json!({
                "source": MULTI_ENTRY_SOURCE,
                "inserter": "inserter-a.example.com",
                "matcher": "matcher-a.example.com",
                "mm": 2,
                "uids": [
                    { "id": "uid-a", "atype": 1, "ext": { "rtiPartner": "example" } },
                    { "id": "uid-b", "atype": EcPartner::default_openrtb_atype() }
                ]
            })
        }

        /// The second entry as bidders see it, with the provenance default.
        fn multi_second_eid() -> serde_json::Value {
            serde_json::json!({
                "source": MULTI_ENTRY_SOURCE,
                "inserter": "default-inserter.example.com",
                "matcher": "default-matcher.example.com",
                "ext": { "segments": ["example"] },
                "uids": [{ "id": "uid-c", "atype": EcPartner::default_openrtb_atype() }]
            })
        }

        /// A row holding `eids` for [`MULTI_ENTRY_SOURCE`], written by the
        /// provider and due for a refresh.
        fn multi_row(eids: Vec<KvEid>) -> KvEntry {
            let mut entry = live_entry();
            entry.ids.insert(
                MULTI_ENTRY_SOURCE.to_owned(),
                KvPartnerId {
                    w: Some(MULTI_PROVIDER.to_owned()),
                    rv: Some("0123456789abcdef".to_owned()),
                    at: Some(1_000),
                    ra: Some(1_500),
                    eids,
                    ..KvPartnerId::default()
                },
            );
            entry
        }

        /// The record under [`MULTI_ENTRY_SOURCE`], with its fresh `rv` and `at`
        /// checked and copied into the expectation of the caller.
        fn fresh_record(row: &KvEntry) -> KvPartnerId {
            let record = row
                .ids
                .get(MULTI_ENTRY_SOURCE)
                .expect("should hold the provider's record")
                .clone();
            let revision = record.rv.as_deref().expect("should carry a revision");
            assert!(
                revision.len() == 16
                    && revision
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "should carry a 16 digit lowercase hex rv"
            );
            assert_ne!(
                revision, "0123456789abcdef",
                "should carry a fresh rv, not the seeded one"
            );
            assert!(record.at.is_some(), "should record when it was acquired");
            record
        }

        #[test]
        fn registered_provider_full_set_reaches_kv_mirror_and_bidstream_in_order() {
            let scenario = multi_scenario(MultiEntryTestProvider::new(), ProviderMode::Hold);
            scenario.seed_live();

            let (_, outcome) = scenario.drive(navigation(Some(&ec_cookie())));

            assert_eq!(outcome, Some(FlushOutcome::Written), "should flush");
            assert_eq!(
                scenario.network.requests_to(MULTI_HOST).len(),
                1,
                "should call the registered provider once"
            );
            assert_writes(&scenario.store, (0, 1, 0), "one flush holds the whole set");
            let record = fresh_record(&scenario.row());
            assert_eq!(
                record,
                KvPartnerId {
                    w: Some(MULTI_PROVIDER.to_owned()),
                    rv: record.rv.clone(),
                    at: record.at,
                    exp: None,
                    ra: None,
                    ru: None,
                    eids: multi_full_set(),
                },
                "should store both entries as returned, written by the provider"
            );

            scenario.store.reset_counts();
            let body = auction_body(Some(&serde_json::json!([browser_entry()])));
            let (response, _) = scenario.drive(auction_request(Some(&ec_cookie()), &body));

            let expected =
                serde_json::json!([browser_entry(), multi_first_eid(), multi_second_eid()]);
            assert_eq!(
                bidder_eids(&bidder_body(&scenario)),
                expected,
                "user.ext.eids should carry the browser entry, then both entries in order"
            );
            assert_eq!(
                header_eids(&response),
                expected,
                "x-ts-eids should carry the same"
            );
            assert_eq!(
                mirror_of(&json_of(response)),
                serde_json::json!({ "v": 1, "eids": [multi_first_eid(), multi_second_eid()] }),
                "the mirror should decode to the same two entries, defaults included"
            );
            assert_eq!(
                scenario.network.requests_to(MULTI_HOST).len(),
                1,
                "control: the /auction should serve from KV without calling the provider"
            );
            assert_no_writes(&scenario.store, "/auction");
        }

        #[test]
        fn new_visitor_hold_answer_is_created_with_the_row_in_one_write() {
            let scenario = multi_scenario(MultiEntryTestProvider::new(), ProviderMode::Hold);
            let mut request = navigation(None);
            *request.uri_mut() = "https://test-publisher.com/"
                .parse()
                .expect("should parse the home page URI");

            let (response, outcome) = scenario.drive(request);

            assert_eq!(
                outcome,
                Some(FlushOutcome::Skipped),
                "should leave nothing for a flush"
            );
            assert_eq!(
                scenario.network.requests_to(MULTI_HOST).len(),
                1,
                "should call the hold-mode provider on the first navigation"
            );
            assert_writes(&scenario.store, (1, 0, 0), "one create holds the answer");
            let ec_id = set_cookie_value(&response, "ts-ec").expect("should set the EC cookie");
            let (row, _) = scenario
                .graph()
                .get(&ec_id)
                .expect("should read the row")
                .expect("should create the row");
            assert_eq!(
                row.ids
                    .values()
                    .find(|record| record.w.as_deref() == Some(MULTI_PROVIDER))
                    .map(|record| record.eids.clone()),
                Some(multi_full_set()),
                "should create the row with the provider's answer"
            );
            assert_eq!(
                bidder_eids(&bidder_body(&scenario)),
                serde_json::json!([multi_first_eid(), multi_second_eid()]),
                "should hand this navigation's auction the answer"
            );
        }

        #[test]
        fn one_entry_one_uid_result_is_written_as_one_entry() {
            let scenario = multi_scenario(MultiEntryTestProvider::new(), ProviderMode::Hold);
            scenario.seed_live();
            answer_with(&scenario, "single");

            scenario.drive(navigation(Some(&ec_cookie())));

            assert_writes(&scenario.store, (0, 1, 0), "one flush");
            let record = fresh_record(&scenario.row());
            assert_eq!(
                record,
                KvPartnerId {
                    w: Some(MULTI_PROVIDER.to_owned()),
                    rv: record.rv.clone(),
                    at: record.at,
                    exp: None,
                    ra: None,
                    ru: None,
                    eids: vec![KvEid {
                        uids: vec![kv_uid(
                            "uid-a",
                            Some(1),
                            Some(serde_json::json!({ "rtiPartner": "example" }))
                        )],
                        inserter: None,
                        matcher: None,
                        mm: None,
                        ext: None,
                    }],
                },
                "should store one entry holding one UID"
            );
            let stored = stored_body(&scenario);
            assert_eq!(
                stored["ids"][MULTI_ENTRY_SOURCE],
                serde_json::json!({
                    "w": MULTI_PROVIDER,
                    "rv": record.rv,
                    "at": record.at,
                    "eids": [{
                        "uids": [{
                            "id": "uid-a",
                            "atype": 1,
                            "ext": { "rtiPartner": "example" }
                        }]
                    }]
                }),
                "should write the one shape of spec 6.1"
            );
        }

        #[test]
        fn later_result_without_an_entry_removes_it() {
            let scenario = multi_scenario(MultiEntryTestProvider::new(), ProviderMode::Hold);
            let mut row = multi_row(multi_full_set());
            row.ids.insert(
                "id5-sync.com".to_owned(),
                KvPartnerId::browser("id5-uid", Some(1_500)),
            );
            scenario.seed(&row);
            answer_with(&scenario, "first");

            let (_, outcome) = scenario.drive(navigation(Some(&ec_cookie())));

            assert_eq!(outcome, Some(FlushOutcome::Written), "should flush");
            assert_eq!(
                scenario.network.requests_to(MULTI_HOST).len(),
                1,
                "control: the due record should have been refreshed"
            );
            let after = scenario.row();
            let record = fresh_record(&after);
            assert_eq!(
                record,
                KvPartnerId {
                    w: Some(MULTI_PROVIDER.to_owned()),
                    rv: record.rv.clone(),
                    at: record.at,
                    exp: None,
                    ra: None,
                    ru: None,
                    eids: multi_first_set(),
                },
                "should leave exactly the returned entry in KV"
            );
            assert_eq!(
                after.ids.get("id5-sync.com"),
                row.ids.get("id5-sync.com"),
                "should keep the record of a source the result does not name"
            );

            let (_, _) = scenario.drive(auction_request(Some(&ec_cookie()), &auction_body(None)));
            assert_eq!(
                bidder_eids(&bidder_body(&scenario)),
                serde_json::json!([
                    multi_first_eid(),
                    { "source": "id5-sync.com", "uids": [{ "id": "id5-uid", "atype": 3 }] }
                ]),
                "should serve exactly the remaining entry, and the other source's record"
            );
        }

        #[test]
        fn stateful_result_with_an_oversized_set_keeps_only_its_removals() {
            let scenario = multi_scenario(MultiEntryTestProvider::stateful(), ProviderMode::Hold);
            let mut row = multi_row(multi_first_set());
            row.ps.insert(
                MULTI_PROVIDER.to_owned(),
                KvProviderState {
                    st: Some(serde_json::json!({ "seen": false })),
                    ..KvProviderState::default()
                },
            );
            scenario.seed(&row);
            answer_with(&scenario, "oversized");

            scenario.drive(navigation(Some(&ec_cookie())));

            assert_eq!(
                scenario.network.requests_to(MULTI_HOST).len(),
                1,
                "control: the provider should have been called"
            );
            let after = scenario.row();
            assert!(
                !after.ids.contains_key(MULTI_ENTRY_SOURCE),
                "should apply the also_invalidate removal and store none of the oversized set"
            );
            let state = after
                .ps
                .get(MULTI_PROVIDER)
                .expect("should keep the provider's row state");
            assert!(
                state.n.is_some_and(|wait_until| wait_until > 1_700_000_000),
                "should back off the rejected answer"
            );
            assert_eq!(
                state,
                &KvProviderState {
                    n: state.n,
                    f: 1,
                    o: None,
                    oc: None,
                    iv: state.iv.clone(),
                    ov: Vec::new(),
                    st: Some(serde_json::json!({ "seen": false })),
                },
                "should keep the old state, not the rejected replacement, and count one failure"
            );
            assert_eq!(
                state.iv.len(),
                2,
                "should record the two invalidated UIDs, so the removal is not undone"
            );
        }

        #[test]
        fn identify_returns_the_first_uid_of_a_full_record() {
            let scenario = multi_scenario(MultiEntryTestProvider::new(), ProviderMode::Hold);
            let mut row = multi_row(multi_full_set());
            row.ids
                .get_mut(MULTI_ENTRY_SOURCE)
                .expect("should hold the record")
                .ra = None;
            row.ids.insert(
                "id5-sync.com".to_owned(),
                KvPartnerId::browser("id5-uid", Some(1_500)),
            );
            scenario.seed(&row);
            let atype = EcPartner::default_openrtb_atype();

            let full = json_of(
                scenario
                    .drive(identify_request(&ec_cookie(), MULTI_ENTRY_SOURCE))
                    .0,
            );
            let one_uid = json_of(
                scenario
                    .drive(identify_request(&ec_cookie(), "id5-sync.com"))
                    .0,
            );

            assert_eq!(full["uid"], "uid-a", "should return the first UID");
            assert_eq!(
                full["eid"],
                serde_json::json!({
                    "source": MULTI_ENTRY_SOURCE,
                    "uids": [{ "id": "uid-a", "atype": atype }]
                }),
                "should return the first UID with the partner's openrtb_atype"
            );
            assert_eq!(
                one_uid["eid"],
                serde_json::json!({
                    "source": "id5-sync.com",
                    "uids": [{ "id": "id5-uid", "atype": atype }]
                }),
                "control: a one-UID record should have the same shape"
            );
            let keys = |body: &serde_json::Value| {
                body.as_object()
                    .expect("should be an object")
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                keys(&full),
                keys(&one_uid),
                "should answer in the same shape"
            );
        }

        #[test]
        fn rollback_to_part_1_config_keeps_the_owners_opt_out() {
            let scenario = Scenario::with_settings(&format!(
                "{AUCTION_TOML}{}",
                partner_toml(MULTI_ENTRY_SOURCE)
            ));
            scenario.network.respond(
                BIDDER_HOST,
                200,
                "application/json",
                r#"{"id":"response","seatbid":[]}"#,
            );
            let mut row = multi_row(multi_full_set());
            row.ps.insert(
                MULTI_PROVIDER.to_owned(),
                KvProviderState {
                    n: Some(FAR_FUTURE),
                    f: 2,
                    o: Some(KvOptOut {
                        at: 1_200,
                        fp: "0".to_owned(),
                    }),
                    oc: Some(1_100),
                    iv: vec!["00112233445566ff".to_owned()],
                    ov: vec!["8899aabbccddeeff".to_owned()],
                    st: Some(serde_json::json!({ "seen": true })),
                },
            );
            scenario.seed(&row);

            let (_, outcome) = scenario.drive(navigation(Some(&returning_with_shared_id())));

            assert_eq!(outcome, Some(FlushOutcome::Written), "should flush");
            let after = scenario.row();
            assert_eq!(
                after.ps.get(MULTI_PROVIDER),
                Some(&KvProviderState {
                    n: None,
                    f: 0,
                    o: Some(KvOptOut {
                        at: 1_200,
                        fp: "0".to_owned(),
                    }),
                    oc: Some(1_100),
                    iv: Vec::new(),
                    ov: vec!["8899aabbccddeeff".to_owned()],
                    st: None,
                }),
                "should keep o, oc and ov and lose n, f, iv and st"
            );
            assert_eq!(
                after.ids.get(MULTI_ENTRY_SOURCE),
                row.ids.get(MULTI_ENTRY_SOURCE),
                "should keep the multi-entry record readable"
            );
            assert_eq!(
                stored_uid(&after, "sharedid.org").as_deref(),
                Some(SHARED_ID),
                "control: the flush should have written the ingest"
            );

            scenario.drive(auction_request(Some(&ec_cookie()), &auction_body(None)));
            let sent = bidder_eids(&bidder_body(&scenario));
            assert!(
                !sent.to_string().contains(MULTI_ENTRY_SOURCE),
                "should not serve a record whose provider the catalog no longer has: {sent}"
            );
            assert!(
                sent.to_string().contains("sharedid.org"),
                "control: should still serve the browser record: {sent}"
            );
        }

        /// A scenario whose Didomi proxy answers `body`, with a capture-only
        /// provider registered through an `IntegrationRegistration`, and a
        /// row holding a browser-written `ssp.example.com` ID.
        fn registered_capture_scenario(body: &str, global: bool) -> Scenario {
            let Scenario {
                mut state,
                store,
                network,
            } = Scenario::with_settings(DIDOMI_TOML);
            if global {
                let owned = Arc::get_mut(&mut state).expect("should own the fresh state");
                Arc::make_mut(&mut owned.settings)
                    .ec
                    .identity
                    .providers
                    .insert(
                        "didomi".to_owned(),
                        ProviderSettings {
                            revoke_scope: Some(trusted_server_core::settings::RevokeScope::Global),
                            ..ProviderSettings::default()
                        },
                    );
            }
            let registration = IntegrationRegistration::builder("didomi")
                .with_identity_provider(
                    trusted_server_core::ec::identity::test_support::answering_capture_provider(
                        "didomi",
                        &["ssp.example.com"],
                        None,
                    ),
                )
                .build();
            let runtime = IdentityRuntime::from_settings_with_providers(
                &state.settings,
                registration.identity_providers,
                BTreeMap::new(),
            )
            .expect("should build the identity runtime");
            let scenario = Scenario {
                state: crate::app::app_state_with_identity_for_test(state, runtime),
                store,
                network,
            };
            scenario
                .network
                .respond("sdk.example.com", 200, "application/json", body);
            let mut entry = live_entry();
            entry.ids.insert(
                "ssp.example.com".to_owned(),
                KvPartnerId {
                    rv: Some("0123456789abcdef".to_owned()),
                    at: Some(1_000),
                    ..KvPartnerId::new("probe-uid")
                },
            );
            scenario.seed(&entry);
            scenario
        }

        #[test]
        fn captured_revocation_writes_with_cas() {
            let clean = registered_capture_scenario(CAPTURE_INVALIDATION, false);
            clean.drive(didomi_navigation(Some(&returning_with_shared_id())));
            assert_writes(
                &clean.store,
                (0, 2, 0),
                "one CAS write for the removal before send, then the writer's flush",
            );
            assert_eq!(
                stored_uid(&clean.row(), "ssp.example.com"),
                None,
                "should remove the captured ID"
            );

            let fifth = registered_capture_scenario(CAPTURE_INVALIDATION, false);
            fifth.store.refuse_conditional_writes(EC_ID, 4);
            fifth.drive(didomi_navigation(Some(&returning_with_shared_id())));
            assert_writes(
                &fifth.store,
                (0, 6, 0),
                "four refused attempts and the fifth before send, no retry after send, the flush",
            );
            assert_eq!(
                stored_uid(&fifth.row(), "ssp.example.com"),
                None,
                "should remove the ID on the fifth attempt"
            );

            let retried = registered_capture_scenario(CAPTURE_INVALIDATION, false);
            retried.store.refuse_conditional_writes(EC_ID, 5);
            retried.drive(didomi_navigation(Some(&returning_with_shared_id())));
            assert_writes(
                &retried.store,
                (0, 7, 0),
                "five refused attempts before send, one more after send, the flush",
            );
            assert_eq!(
                stored_uid(&retried.row(), "ssp.example.com"),
                None,
                "should remove the ID on the attempt after send"
            );
        }

        #[test]
        fn global_revoke_from_real_browser_writes_as_withdrawal() {
            let withdrawn = Scenario::new();
            withdrawn.seed_live();
            drive_with_runtime(
                &withdrawn.state,
                navigation(Some(&ec_cookie())),
                Some(&withdrawn.state.identity),
                withdraw,
            );
            let withdrawal_writes = withdrawn.store.writes();

            let scenario = registered_capture_scenario(r#"{"opted_out":true}"#, true);

            let (response, _) =
                scenario.drive(didomi_navigation(Some(&returning_with_shared_id())));

            let row = scenario.row();
            assert!(!row.consent.ok, "should tombstone the row");
            assert!(row.ids.is_empty(), "should remove every ID");
            assert_eq!(
                scenario.row().consent.ok,
                withdrawn.row().consent.ok,
                "should end as a consent withdrawal does"
            );
            let writes = scenario.store.writes();
            assert_eq!(
                (writes.add, writes.if_generation_match, writes.overwrite),
                (
                    withdrawal_writes.add,
                    withdrawal_writes.if_generation_match,
                    withdrawal_writes.overwrite
                ),
                "should write the tombstone and its marker as a withdrawal does"
            );
            assert!(
                set_cookies(&response).iter().any(|cookie| {
                    cookie_name(cookie) == "ts-ec" && cookie.contains("Max-Age=0")
                }),
                "should expire the EC cookie"
            );
        }

        /// A proxied navigation from a client that fails the real-browser
        /// check: no `Sec-Fetch-Dest`, a non-browser user agent.
        fn non_browser_didomi_navigation() -> HttpRequest {
            let mut request = didomi_navigation(Some(&returning_with_shared_id()));
            request.extensions_mut().clear();
            request.headers_mut().remove("sec-fetch-dest");
            request
                .headers_mut()
                .insert("user-agent", HeaderValue::from_static("curl/8.0"));
            request
        }

        #[test]
        fn non_browser_capture_writes_its_removal() {
            let scenario = registered_capture_scenario(CAPTURE_INVALIDATION, false);
            assert!(
                scenario.row().ids.contains_key("ssp.example.com"),
                "control: the seeded row holds the record"
            );

            let (response, _) = scenario.drive(non_browser_didomi_navigation());

            assert_eq!(response.status(), 200, "should serve the proxied answer");
            assert!(
                !scenario.row().ids.contains_key("ssp.example.com"),
                "should apply the removal whatever the client"
            );
            assert!(
                !set_cookies(&response)
                    .iter()
                    .any(|cookie| cookie_name(cookie) == "ts-ec" && cookie.contains("Max-Age=0")),
                "should not expire the EC cookie"
            );
        }

        #[test]
        fn non_browser_global_revoke_writes_as_withdrawal() {
            let scenario = registered_capture_scenario(r#"{"opted_out":true}"#, true);

            let (response, _) = scenario.drive(non_browser_didomi_navigation());

            assert!(!scenario.row().consent.ok, "should tombstone the row");
            assert!(
                set_cookies(&response).iter().any(|cookie| {
                    cookie_name(cookie) == "ts-ec" && cookie.contains("Max-Age=0")
                }),
                "should expire the EC cookie"
            );
        }

        #[test]
        fn returning_navigation_answered_404_flushes_only_hold_removals_and_backoff() {
            let removal_row = || multi_row(multi_first_set());
            let drive_home = |scenario: &Scenario, status: u16| {
                scenario.network.respond(
                    ORIGIN_HOST,
                    status,
                    "text/html",
                    "<html><head></head><body>page</body></html>",
                );
                let mut request = navigation(Some(&ec_cookie()));
                *request.uri_mut() = "https://test-publisher.com/"
                    .parse()
                    .expect("should parse the home page URI");
                scenario.drive(request)
            };

            let ok = multi_scenario(MultiEntryTestProvider::stateful(), ProviderMode::Hold);
            ok.seed(&removal_row());
            answer_with(&ok, "oversized");
            drive_home(&ok, 200);
            assert!(
                !ok.row().ids.contains_key(MULTI_ENTRY_SOURCE),
                "control: a 200 should flush the removal"
            );

            let not_found = multi_scenario(MultiEntryTestProvider::stateful(), ProviderMode::Hold);
            not_found.seed(&removal_row());
            answer_with(&not_found, "oversized");
            drive_home(&not_found, 404);

            assert_eq!(
                not_found.network.requests_to(MULTI_HOST).len(),
                1,
                "should still have run the hold call"
            );
            let after = not_found.row();
            assert!(
                !after.ids.contains_key(MULTI_ENTRY_SOURCE),
                "should flush the hold answer's removal on a 404"
            );
            assert_eq!(
                after.ps.get(MULTI_PROVIDER).map(|state| state.iv.len()),
                Some(2),
                "should flush the removal's record of the two invalidated UIDs"
            );

            let backoff = multi_scenario(MultiEntryTestProvider::new(), ProviderMode::Hold);
            backoff.seed_live();
            answer_with(&backoff, "none");
            drive_home(&backoff, 404);
            assert!(
                backoff
                    .row()
                    .ps
                    .get(MULTI_PROVIDER)
                    .and_then(|state| state.n)
                    .is_some_and(|wait_until| wait_until > 1_700_000_000),
                "should flush the no_match backoff on a 404"
            );

            let positive = multi_scenario(MultiEntryTestProvider::new(), ProviderMode::Hold);
            positive.seed_live();
            answer_with(&positive, "full");
            drive_home(&positive, 404);
            assert!(
                !positive.row().ids.contains_key(MULTI_ENTRY_SOURCE),
                "should store no positive hold answer on a 404"
            );
        }

        /// A scenario like [`Scenario::new`] with `MultiEntryTestProvider`
        /// registered on its own source, and its endpoint left unanswered so
        /// the provider writes nothing the golden tests would see.
        fn golden_registered_scenario() -> Scenario {
            let scenario = registered_scenario(
                vec![multi_registration(MultiEntryTestProvider::new())],
                ProviderMode::AfterSend,
            );
            scenario.network.clear_reply(MULTI_HOST);
            scenario
        }

        #[test]
        fn golden_browser_ingest_with_a_registered_provider_writes_the_first_uid() {
            let cookie = format!("ts-eids={GOLDEN_INGEST_COOKIE}; sharedId=shared-1");

            let returning = golden_registered_scenario();
            returning.seed_live();
            let (_, outcome) =
                returning.drive(navigation(Some(&format!("ts-ec={EC_ID}; {cookie}"))));
            assert_eq!(outcome, Some(FlushOutcome::Written), "should flush");
            assert_writes(&returning.store, (0, 1, 0), "one flush");
            assert_eq!(
                assert_one_uid_records(&returning.row()),
                golden_expected_ingest(),
                "the flushed row should hold the UIDs the code before this spec stored"
            );

            let new_visitor = golden_registered_scenario();
            let (response, _) = new_visitor.drive(navigation(Some(&cookie)));
            let minted = set_cookie_value(&response, "ts-ec").expect("should mint an EC");
            assert_writes(&new_visitor.store, (1, 0, 0), "one create");
            assert_eq!(
                assert_one_uid_records(&new_visitor.row_of(&minted)),
                golden_expected_ingest(),
                "the created row should hold the same UIDs"
            );
        }

        #[test]
        fn golden_one_uid_row_with_a_registered_provider_goes_out_unchanged() {
            let scenario = golden_registered_scenario();
            let mut entry = live_entry();
            for (source, uid) in [("id5-sync.com", "id5-uid"), ("sharedid.org", "shared-uid")] {
                entry
                    .ids
                    .insert(source.to_owned(), KvPartnerId::browser(uid, Some(1_500)));
            }
            scenario.seed(&entry);

            let (response, _) =
                scenario.drive(auction_request(Some(&ec_cookie()), &auction_body(None)));

            assert_eq!(
                bidder_eids(&bidder_body(&scenario)),
                serde_json::json!([
                    { "source": "id5-sync.com", "uids": [{ "id": "id5-uid", "atype": 3 }] },
                    { "source": "sharedid.org", "uids": [{ "id": "shared-uid", "atype": 3 }] }
                ]),
                "user.ext.eids should equal what the code before this spec sent"
            );
            assert_eq!(
                response
                    .headers()
                    .get("x-ts-eids")
                    .and_then(|value| value.to_str().ok()),
                Some(GOLDEN_ROW_HEADER),
                "x-ts-eids should equal what the code before this spec built, byte for byte"
            );
        }

        #[test]
        fn golden_auction_body_forwarding_with_a_registered_provider_is_unchanged() {
            let scenario = golden_registered_scenario();
            scenario.seed_live();
            let eids: serde_json::Value =
                serde_json::from_str(GOLDEN_BODY_EIDS).expect("should parse the body EIDs");
            let forwarded: serde_json::Value = serde_json::from_str(GOLDEN_BODY_FORWARDED)
                .expect("should parse the forwarded EIDs");

            scenario.drive(auction_request(
                Some(&ec_cookie()),
                &auction_body(Some(&eids)),
            ));

            assert_eq!(
                bidder_eids(&bidder_body(&scenario)),
                forwarded,
                "user.ext.eids should equal what the code before this spec forwarded"
            );
            assert_no_writes(&scenario.store, "/auction");
        }
    }
}
