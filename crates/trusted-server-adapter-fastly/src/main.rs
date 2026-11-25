// Unit tests may buffer bodies with `Body::into_bytes`; production code must not.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

use edgezero_adapter_fastly::context::FastlyOutboundObservation;
use edgezero_adapter_fastly::request::{
    send_assembly_error, send_request_with_registries_and_hooks,
};
use edgezero_core::app::Hooks as _;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{Extensions, Request as HttpRequest, Response as HttpResponse};
use edgezero_core::logging::ManagedLoggerError;
use edgezero_core::time::MonotonicClock;
use error_stack::Report;
use fastly::http::Method as FastlyMethod;
use fastly::{Request as FastlyRequest, Response as FastlyResponse};

use trusted_server_core::cache_policy::{EdgeCacheHeader, cache_control_headers_have_directive};
use trusted_server_core::constants::HEADER_X_TS_VERSION;
use trusted_server_core::ec::device::DeviceSignals;
use trusted_server_core::ec::finalize::ec_finalize_response;
use trusted_server_core::ec::kv::KvIdentityGraph;
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::forwarder::ForwarderPreparation;
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::integrations::RequestFilterEffects;
use trusted_server_core::platform::ClientInfo;
use trusted_server_core::proxy::AssetProxyCachePolicy;
use trusted_server_core::response_privacy::TerminalPrivateResponse;
use trusted_server_core::settings::Settings;
use trusted_server_core::trace::{TraceTerminalResponse, is_trace_path};
use trusted_server_core::version_header::git_version_header_value;

mod app;
mod compat;
#[cfg(test)]
mod deadline_network;
mod delivery;
mod ec_kv;
mod esi_assembly;
mod management_api;
mod middleware;
mod platform;
mod post_delivery;
mod rate_limiter;
mod sandbox;
mod template_cache;
mod tinybird;

use crate::app::{
    EcFinalizeState, RuntimeStoreConfig, TrustedServerApp, assemble_app,
    load_settings_from_config_store,
};
use crate::delivery::handle_delivery;
use crate::ec_kv::FastlyEcKvStore;
use crate::middleware::{
    HEADER_X_TS_FINALIZED, apply_finalize_headers, per_request_synthesized_headers,
    resolve_geo_for_response,
};
use crate::platform::{RequestGeo, client_info_from_request, resolve_client_ip};
use crate::post_delivery::{FinalizeState, run_post_delivery};
use crate::sandbox::{
    LogContext, RetainedApp, Sandbox, SandboxCounters, ServeMode, StartupDiagnostics, instance_id,
    metrics_enabled,
};
use crate::tinybird::TelemetryQueue;
// Only the reuse path builds a serving loop, so the retirement snapshots have
// no consumer in the default build.
#[cfg(feature = "reusable-sandbox")]
use crate::sandbox::RetirementCounters;

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
/// `#[fastly::main]` so the entry point owns the sandbox lifecycle. Each
/// request goes to [`handle_request`], which sends it through `EdgeZero`'s
/// canonical Fastly lifecycle.
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
            // `run_custom` installs `EdgeZero`'s managed logger from
            // `TrustedServerApp::logging_for("fastly")` before the callback, and
            // completes the callback's SDK result exactly once. The callback
            // always returns `Ok`: `EdgeZero` or the callback has already
            // answered, so there is no error for the SDK to turn into a second
            // response. Only a failed logger installation returns `Err`.
            let result = edgezero_adapter_fastly::lifecycle::run_custom::<TrustedServerApp, _, _, _>(
                FastlyRequest::from_client(),
                |request, sandbox: &mut Sandbox| {
                    handle_request(request, sandbox, &mut startup);
                    Ok::<(), ManagedLoggerError>(())
                },
            );
            if let Err(error) = result {
                report_logger_failure(error);
            }
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

    // `serve_custom` installs `EdgeZero`'s managed logger before each callback,
    // as `run_custom` does.
    let summary = edgezero_adapter_fastly::lifecycle::serve_custom::<TrustedServerApp, _, _, _>(
        fastly::http::serve::Serve::new()
            .with_max_requests(limits.max_requests)
            .with_max_memory(limits.max_memory_mib)
            .with_max_lifetime(limits.max_lifetime)
            .with_timeout(limits.timeout),
        |request, sandbox: &mut Sandbox| {
            counters.observe(sandbox, |sandbox| {
                handle_request(request, sandbox, &mut startup);
            });
            // An `Err` would end serving with no response, so the callback
            // reports none, whatever happened to this request's delivery.
            Ok::<(), ManagedLoggerError>(())
        },
    );

    log::info!(
        "sandbox retiring after {} attempted callback(s), {} observed, {} build attempt(s)",
        summary.requests(),
        counters.requests(),
        counters.attempts()
    );
    if let Some(error) = summary.error() {
        report_logger_failure(*error);
    }
}

/// Reports that `EdgeZero` could not install its managed logger.
///
/// `EdgeZero` stops before the callback when installation fails, so the error
/// cannot go through `log`.
fn report_logger_failure(error: ManagedLoggerError) {
    #[allow(clippy::print_stderr, reason = "the managed logger is not installed")]
    {
        eprintln!("managed logger installation failed: {error}");
    }
}

#[cfg(not(feature = "reusable-sandbox"))]
fn serve_loop(_limits: crate::sandbox::SandboxLimits, _startup: StartupDiagnostics) {
    unreachable!("serve_mode never selects reuse without the reusable-sandbox feature")
}

/// Handles one request end to end.
///
/// Native short-circuits answer first. Every other request is assembled into
/// the sandbox's retained app and sent through `EdgeZero`, which owns the
/// response; [`handle_delivery`] then decides whether anything is left to do,
/// and [`log_outbound_observation`] logs the request's backend registrations.
///
/// Every non-panicking path through this function must answer exactly once.
/// In a reused sandbox the SDK refuses to wait for the next request until the
/// current one is complete, so a missed answer stalls the loop rather than
/// merely dropping one response.
fn handle_request(req: FastlyRequest, sandbox: &mut Sandbox, startup: &mut StartupDiagnostics) {
    // Taken at entry, so the assembly-error response and every deadline share
    // one clock domain with `EdgeZero`'s.
    let clock = MonotonicClock::default();
    let request_start = clock.now();
    // The framework counts the callback before invoking it, including early
    // returns, so this is already this request's 1-based ordinal.
    let ordinal = sandbox.requests();
    // Classified before any native mutation, and before the health probe, so
    // trace-path decisions describe the request as received.
    let trace_path = is_trace_path(req.get_path());

    // Health probe bypasses settings and app construction as a cheap liveness signal.
    if !trace_path && let Some(response) = health_response(&req) {
        response.send_to_client();
        return;
    }

    // `EdgeZero` installed its managed logger before this callback, so the
    // messages held since startup can go out now.
    startup.flush();

    // Correlation is request-local and never retained. `FASTLY_TRACE_ID` names
    // the sandbox, not the request, so it is not used here. The id rides on the
    // response only when metrics are enabled; the client request is left
    // untouched so nothing new reaches origin in the default configuration.
    let request_id = req
        .get_client_request_id()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}-{ordinal}", instance_id()));

    // Short-circuit the sandbox counters probe before app construction. It must
    // not build the application: polling it would otherwise increment the very
    // build counter it reports.
    #[cfg(feature = "reusable-sandbox")]
    if req.get_method() == FastlyMethod::GET && req.get_path() == sandbox::SANDBOX_METRICS_PATH {
        match load_settings_from_config_store(&RuntimeStoreConfig::logical()) {
            Ok(settings) if metrics_enabled(&settings) => {
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
        match load_settings_from_config_store(&RuntimeStoreConfig::logical()) {
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

    // Assemble lazily, once per sandbox. Reached only past the health, JA4, and
    // counters short-circuits, so none of those pays for construction.
    // `initialize` retains only success and returns the error unchanged, so a
    // transient config-store failure answers this request and the next
    // callback retries assembly.
    let assembled = sandbox.initialize(|| {
        assemble_app(
            load_settings_from_config_store(&RuntimeStoreConfig::logical()),
            &LogContext::new(ordinal, &request_id),
        )
    });
    let counters = SandboxCounters::capture(sandbox, ordinal, &request_id);
    match assembled.and_then(|()| retained_app(sandbox)) {
        Ok(retained) => serve(req, retained, &counters, trace_path),
        Err(error) => handle_delivery(
            send_assembly_error(req, error, request_start, clock),
            &counters,
            |()| {},
        ),
    }
    log_outbound_observation();
}

/// Logs this callback's backend registration counts at info.
///
/// Runs after delivery and post-delivery work, so pull-sync and telemetry
/// registrations count. Hosted Fastly logs at info, so every request that
/// reached `EdgeZero` leaves one line.
fn log_outbound_observation() {
    if let Some(observation) = FastlyOutboundObservation::current() {
        let stats = observation.snapshot();
        log::info!(
            target: "trusted_server::outbound_observation",
            "configurations={} registrations={} total_registration_us={} max_registration_us={}",
            stats.configuration_count(),
            stats.registration_count(),
            stats.total_registration_duration().as_micros(),
            stats.max_registration_duration().as_micros()
        );
    }
}

/// The app a successful `initialize` retained.
///
/// # Errors
///
/// Returns an internal [`EdgeError`] if the sandbox retained nothing, which
/// the entry point answers like any other assembly error.
fn retained_app(sandbox: &Sandbox) -> Result<&RetainedApp, EdgeError> {
    sandbox.state().ok_or_else(|| {
        EdgeError::internal(std::io::Error::other(
            "no application was retained after a successful build",
        ))
    })
}

/// Runs the native request work, sends the request through `EdgeZero`, and handles its delivery.
///
/// The native work, [`ClientRequestValues::read`], reads what only the SDK
/// request exposes before `EdgeZero` converts it. `prepare` moves those values
/// into the request extensions, with the telemetry queue and this request's
/// synthesized headers. `finalize` takes over the post-routing steps.
/// Post-delivery work runs only after a committed application body.
fn serve(
    mut req: FastlyRequest,
    retained: &RetainedApp,
    counters: &SandboxCounters,
    trace_path: bool,
) {
    let settings = retained.state.settings.as_ref();
    let values = ClientRequestValues::read(&mut req, settings, trace_path);
    // The request's one geo lookup serves `finalize` here and, from the
    // request extensions, every later step that uses geo.
    let request_geo = values.request_geo.clone();
    let queue = TelemetryQueue::default();

    let prepare = |_req: &mut FastlyRequest, extensions: &mut Extensions| {
        values.prepare(extensions, settings, &queue);
    };
    let inputs = FinalizeInputs {
        settings,
        geo: request_geo.geo(),
        counters,
    };
    let delivery = send_request_with_registries_and_hooks(
        &retained.app,
        TrustedServerApp::stores(),
        req,
        prepare,
        |response: &mut HttpResponse| finalize_response(response, &inputs),
    );
    handle_delivery(delivery, counters, |state| {
        run_post_delivery(state, &queue, settings, &counters.log_context());
    });
}

/// What the native work reads from the SDK client request before `EdgeZero` converts it.
///
/// Only the client request exposes the trusted forwarder, the resolved client
/// IP, TLS metadata, and device signals.
struct ClientRequestValues {
    forwarder: ForwarderPreparation,
    client_info: ClientInfo,
    device_signals: Option<DeviceSignals>,
    /// The geo lookup for `client_info`'s client IP.
    request_geo: RequestGeo,
}

impl ClientRequestValues {
    /// Reads the client request's values and sanitizes `req` for dispatch.
    ///
    /// A trace path keeps its forwarded and TLS headers, and reads no device
    /// signals.
    fn read(req: &mut FastlyRequest, settings: &Settings, trace_path: bool) -> Self {
        let trusted_client_ip = settings.trusted_client_ip.as_ref();

        // Preserve the public origin before native sanitation removes its source
        // fields. The frozen decision carries no authentication material.
        let forwarder = compat::capture_fastly_forwarder(req, settings);

        // Resolve the trusted client IP, then strip client-spoofable forwarded
        // headers before dispatch. One call keeps resolution ahead of the
        // sanitization that removes the headers it reads.
        let resolved_client_ip = if trace_path {
            resolve_client_ip(req, req.get_client_ip_addr(), trusted_client_ip)
        } else {
            compat::resolve_and_sanitize_client_ip(req, trusted_client_ip)
        };

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

        // These accessors only return real values on the client request, so they
        // are read here and carried in the request extensions for
        // build_per_request_services and EC bot classification.
        let client_info = client_info_from_request(req, resolved_client_ip);
        let device_signals = (!trace_path).then(|| derive_device_signals(req));
        // The request's one geo lookup, for the resolved client IP. The
        // synthesized headers, `finalize`, the finalize middleware, the trace
        // metadata and the per-request services all read it.
        let request_geo = RequestGeo::lookup_once(client_info.client_ip);
        Self {
            forwarder,
            client_info,
            device_signals,
            request_geo,
        }
    }

    /// Moves the values into `extensions`, with `queue` and the request's synthesized headers.
    fn prepare(self, extensions: &mut Extensions, settings: &Settings, queue: &TelemetryQueue) {
        extensions.insert(self.forwarder);
        if let Some(device_signals) = self.device_signals {
            extensions.insert(device_signals);
        }
        extensions.insert(self.client_info);
        extensions.insert(queue.clone());
        extensions.insert(per_request_synthesized_headers(
            settings,
            self.request_geo.geo(),
        ));
        extensions.insert(self.request_geo);
    }
}

/// What the `finalize` step reads besides the routed response.
struct FinalizeInputs<'a> {
    settings: &'a Settings,
    /// The geo lookup the entry point ran once for this request.
    geo: Option<&'a GeoInfo>,
    counters: &'a SandboxCounters,
}

/// Takes over the post-routing steps for a routed response, before egress policy and framing.
///
/// `EdgeZero` runs it for every routed response, handler errors and its
/// canonical 404 and 405 included. Detached responses never reach it. A trace
/// response is returned untouched. Otherwise it applies the finalize headers
/// unless the middleware already did, the asset cache policy, EC
/// finalization, the terminal privacy effects, and, with metrics enabled, the
/// sandbox counters.
///
/// Returns the pull-sync inputs post-delivery work needs, or `None` when the
/// response carries no EC state or its partner registry cannot be built.
fn finalize_response(
    response: &mut HttpResponse,
    inputs: &FinalizeInputs<'_>,
) -> Option<FinalizeState> {
    // Trace responses keep their exact hardened headers; no finalizer touches them.
    if response
        .extensions()
        .get::<TraceTerminalResponse>()
        .is_some()
    {
        return None;
    }

    let ec_state = response.extensions_mut().remove::<EcFinalizeState>();
    let asset_cache_policy = response.extensions_mut().remove::<AssetProxyCachePolicy>();
    let request_filter_effects = response.extensions_mut().remove::<RequestFilterEffects>();

    if !take_finalize_sentinel(response) {
        let geo = resolve_geo_for_response(response, inputs.geo);
        apply_finalize_headers(inputs.settings, geo, response);
    }

    if let Some(policy) = asset_cache_policy {
        policy.apply_after_route_finalization(response, EdgeCacheHeader::SurrogateControl);
    }

    let state = ec_state.and_then(|mut ec_state| {
        match apply_edgezero_ec_finalize(inputs.settings, &mut ec_state, response) {
            Ok(partner_registry) => Some(FinalizeState {
                partner_registry,
                ec_state,
            }),
            Err(e) => {
                log::error!(
                    "EC finalize skipped: failed to build partner registry: {e:?}{}",
                    inputs.counters.log_context()
                );
                None
            }
        }
    });

    apply_terminal_response_effects(response, request_filter_effects.as_ref());
    if metrics_enabled(inputs.settings) {
        attach_sandbox_counters(response, inputs.counters);
    }
    state
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
/// Called from `finalize`, before headers are committed. The counters
/// therefore describe the request up to commitment and cannot report its
/// eventual outcome; a failure after commitment is recorded in logs instead,
/// with the same counters, and reconciled during analysis.
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

fn take_finalize_sentinel(response: &mut HttpResponse) -> bool {
    response
        .headers_mut()
        .remove(HEADER_X_TS_FINALIZED)
        .is_some()
}

fn apply_edgezero_ec_finalize(
    settings: &Settings,
    ec_state: &mut EcFinalizeState,
    response: &mut HttpResponse,
) -> Result<PartnerRegistry, Report<TrustedServerError>> {
    let partner_registry = PartnerRegistry::from_config(&settings.ec.partners)?;
    let finalize_kv_graph = if ec_state.use_finalize_kv {
        maybe_identity_graph(settings)
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
        response,
    );
    Ok(partner_registry)
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

pub(crate) fn maybe_identity_graph(settings: &Settings) -> Option<KvIdentityGraph> {
    settings
        .ec
        .ec_store
        .as_ref()
        .map(|store_name| KvIdentityGraph::new(FastlyEcKvStore::new(store_name)))
}

/// Constructs a `KvIdentityGraph` from settings, or returns an error if the
/// `ec_store` config is not set.
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use base64::Engine as _;
    use edgezero_adapter_fastly::request::FastlyService;
    use edgezero_core::app::App;
    use edgezero_core::body::Body as EdgeBody;
    use edgezero_core::context::RequestContext;
    use edgezero_core::http::response_builder;
    use edgezero_core::http::{HeaderValue, Method, StatusCode};
    use edgezero_core::ingress::{
        CapturedTarget, HeaderFidelity, InboundOrigin, OriginSource, RequestIngress,
        TargetUnavailable,
    };
    use edgezero_core::pre_dispatch::PreDispatchRequest;
    use edgezero_core::router::{PreDispatchHook, RouterService};
    use fastly::mime;
    use std::net::{IpAddr, Ipv4Addr};

    use trusted_server_core::integrations::HeaderMutation;
    use trusted_server_core::redacted::Redacted;
    use trusted_server_core::settings::TrustedClientIpConfig;
    use trusted_server_core::test_support::logging::LogCapture;
    use trusted_server_core::test_support::scripted_client::ScriptedHttpClient;
    use trusted_server_core::trace::TracePreDispatchHook;

    use crate::delivery::tests::{committed, committed_then_failed};

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

    /// [`test_settings`] with the trace page on, behind fictional credentials.
    fn trace_settings() -> Settings {
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
        // Counters would land on a private, no-store response, so a trace
        // response that kept its headers proves `finalize` never reached it.
        settings.debug.sandbox_metrics_enabled = true;
        settings
    }

    fn counters() -> SandboxCounters {
        SandboxCounters {
            ordinal: 2,
            builds: 1,
            request_id: "request-example".to_owned(),
        }
    }

    /// Request filter effects that would break a trace response's contract.
    fn hostile_effects() -> RequestFilterEffects {
        RequestFilterEffects {
            request_headers: Vec::new(),
            response_headers: vec![
                HeaderMutation::set("content-security-policy", "default-src *"),
                HeaderMutation::set("content-type", "text/plain"),
                HeaderMutation::append("set-cookie", "example=operator"),
                HeaderMutation::set("cache-control", "public, max-age=3600"),
            ],
        }
    }

    #[test]
    fn trace_response_passes_untouched() {
        let settings = trace_settings();
        let (mut request, _body) = edgezero_core::http::request_builder()
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
            .expect("should build protected asset request")
            .into_parts();
        let trusted_server_core::trace::TracePreflight::Ready(dispatch) =
            trusted_server_core::trace::preflight(&settings, &mut request)
        else {
            panic!("should accept protected fixed asset");
        };
        let mut response = dispatch.fixed_asset(b"example immutable asset");
        response.extensions_mut().insert(hostile_effects());
        let original = response.headers().clone();
        let counters = counters();

        let state = finalize_response(
            &mut response,
            &FinalizeInputs {
                settings: &settings,
                geo: None,
                counters: &counters,
            },
        );

        assert!(state.is_none(), "should hand no pull-sync inputs on");
        assert_eq!(
            response.headers(),
            &original,
            "should bypass every ordinary finalizer and preserve the protected digest"
        );
        assert!(
            response.headers().get("set-cookie").is_none(),
            "should never acquire an operator cookie"
        );
    }

    /// Drives `request` through `hook` on a router whose routes record a continuation.
    ///
    /// # Panics
    ///
    /// Panics when the hook continues to a route instead of answering.
    fn dispatch_through_hook(
        hook: &Arc<TracePreDispatchHook>,
        request: HttpRequest,
    ) -> HttpResponse {
        let continued = Arc::new(AtomicBool::new(false));
        let route_continued = Arc::clone(&continued);
        let route = move |_ctx: RequestContext| {
            route_continued.store(true, Ordering::SeqCst);
            async { Ok::<_, EdgeError>(HttpResponse::new(EdgeBody::empty())) }
        };
        let method = request.method().clone();
        let hook = Arc::clone(hook);
        let router = RouterService::builder()
            .pre_dispatch_hook_arc(hook)
            .route("/", method.clone(), route.clone())
            .route("/{*rest}", method, route)
            .build();
        let response = futures::executor::block_on(router.oneshot(request))
            .expect("should build local trace response");
        assert!(
            !continued.load(Ordering::SeqCst),
            "should intercept reserved request"
        );
        response
    }

    #[test]
    fn trace_terminal_actions_errors_and_head_pass_finalize_untouched() {
        let settings = trace_settings();
        let hook = Arc::new(TracePreDispatchHook::new(
            Arc::new(settings.clone()),
            Arc::new(|_| panic!("should never query metadata for actions, errors or HEAD")),
        ));
        let counters = counters();
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
            let mut response = dispatch_through_hook(&hook, request);
            assert_eq!(
                response.status(),
                expected,
                "should preserve action and preflight status"
            );
            response.extensions_mut().insert(hostile_effects());
            let original = response.headers().clone();

            let state = finalize_response(
                &mut response,
                &FinalizeInputs {
                    settings: &settings,
                    geo: None,
                    counters: &counters,
                },
            );

            assert!(state.is_none(), "should hand no pull-sync inputs on");
            assert_eq!(
                response.headers(),
                &original,
                "should bypass hostile ordinary finalizers for every terminal response"
            );
            assert_eq!(
                response.headers().get_all("set-cookie").iter().count(),
                usize::from(action.is_some()),
                "should emit only one deliberate action cookie and none on reads or errors"
            );
            if head {
                let body = futures::executor::block_on(response.into_body().into_bytes_bounded(1))
                    .expect("should read the trace HEAD body");
                assert!(body.is_empty(), "should keep the trace HEAD bodiless");
            }
        }
    }

    /// Records the path every request reaches the pre-dispatch hook with.
    struct PathRecorder(Arc<Mutex<Vec<String>>>);

    #[async_trait::async_trait(?Send)]
    impl PreDispatchHook for PathRecorder {
        async fn handle(
            &self,
            request: &mut PreDispatchRequest,
        ) -> Result<Option<HttpResponse>, EdgeError> {
            self.0
                .lock()
                .expect("should record the hook path")
                .push(request.uri().path().to_owned());
            Ok(Some(HttpResponse::new(EdgeBody::empty())))
        }
    }

    #[test]
    fn trace_path_classification_survives_canonical_request_conversion() {
        // The entry point skips client-IP sanitization when the native path is a
        // trace path; the trace hook later classifies the path `EdgeZero`'s
        // canonical conversion hands it. Both must agree, or unsanitized
        // forwarded headers could reach ordinary dispatch.
        let paths = Arc::new(Mutex::new(Vec::new()));
        let app = App::from_router::<TrustedServerApp>(
            RouterService::builder()
                .pre_dispatch_hook_arc(Arc::new(PathRecorder(Arc::clone(&paths))))
                .build(),
            edgezero_adapter_fastly::FASTLY_PLATFORM,
        )
        .expect("should assemble the recording app");
        let mut hooked = Vec::new();
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

            let _delivery = FastlyService::new(&app).run_request_with_hooks_for_test(
                req,
                |_req, _extensions| {},
                |_| (),
                committed,
            );

            // Admission may refuse an odd target before the hook runs, which
            // keeps it out of ordinary dispatch too.
            let reached = paths.lock().expect("should read the hook paths").pop();
            if let Some(path) = reached {
                assert_eq!(
                    is_trace_path(&path),
                    native,
                    "should classify {url} identically before and after conversion"
                );
                hooked.push(url);
            }
        }
        for url in [
            "https://example.com/_ts/trace",
            "https://example.com/_ts/trace/enable?source=example",
            "https://example.com/auction",
            "https://example.com/",
        ] {
            assert!(
                hooked.contains(&url),
                "should reach the pre-dispatch hook with {url}"
            );
        }
    }

    #[test]
    fn reused_sandbox_keeps_serving_after_delivery_failure() {
        let mut sandbox = Sandbox::default();
        let settings = test_settings();
        let mut statuses = Vec::new();
        let mut post_delivery = 0;

        // Two requests through one sandbox, each the way `handle_request`
        // runs them. The first fails after commitment, which must neither
        // end serving nor drop the retained app.
        for request in 0..2 {
            sandbox
                .initialize(|| assemble_app(Ok(test_settings()), &""))
                .expect("should assemble or reuse the app");
            let retained = retained_app(&sandbox).expect("should retain the app");
            let counters = SandboxCounters::capture(&sandbox, request + 1, "request-example");
            let inputs = FinalizeInputs {
                settings: &settings,
                geo: None,
                counters: &counters,
            };

            let delivery = FastlyService::new(&retained.app).run_request_with_hooks_for_test(
                FastlyRequest::get("https://publisher.example.com/static/tsjs=tsjs-unified.min.js"),
                |_req, extensions| {
                    extensions.insert(ClientInfo::default());
                    extensions.insert(RequestGeo::default());
                },
                |response: &mut HttpResponse| {
                    statuses.push(response.status());
                    finalize_response(response, &inputs)
                },
                |envelope| {
                    if request == 0 {
                        committed_then_failed(
                            envelope,
                            edgezero_adapter_fastly::delivery::DeliveryError::Transport,
                        )
                    } else {
                        committed(envelope)
                    }
                },
            );
            handle_delivery(delivery, &counters, |_state| post_delivery += 1);
        }

        assert_eq!(
            statuses,
            [StatusCode::OK, StatusCode::OK],
            "should route both requests through the retained app"
        );
        assert_eq!(
            sandbox.initialization_attempts(),
            1,
            "should serve the second request from the first build"
        );
        assert_eq!(
            post_delivery, 2,
            "should run post-delivery work after each committed application body, the one that failed after commitment included"
        );
    }

    #[test]
    fn skipped_operator_header_warns_once_per_request() {
        let mut settings = test_settings();
        settings
            .response_headers
            .insert("connection".to_owned(), "x example".to_owned());
        let mut sandbox = Sandbox::default();
        sandbox
            .initialize(|| assemble_app(Ok(settings.clone()), &""))
            .expect("should assemble the app");
        let retained = retained_app(&sandbox).expect("should retain the app");
        let counters = SandboxCounters::capture(&sandbox, 1, "request-example");
        let inputs = FinalizeInputs {
            settings: &settings,
            geo: None,
            counters: &counters,
        };

        // A routed response, which the finalize middleware finalizes, and a
        // canonical 405, which only the entry point's finalize step reaches.
        for (label, request) in [
            (
                "routed",
                FastlyRequest::get("https://publisher.example.com/static/tsjs=tsjs-unified.min.js"),
            ),
            (
                "canonical 405",
                FastlyRequest::new(FastlyMethod::TRACE, "https://publisher.example.com/"),
            ),
        ] {
            let capture = LogCapture::start();
            let delivery = FastlyService::new(&retained.app).run_request_with_hooks_for_test(
                request,
                |_req, extensions| {
                    extensions.insert(per_request_synthesized_headers(&settings, None));
                    extensions.insert(ClientInfo::default());
                    extensions.insert(RequestGeo::default());
                },
                |response: &mut HttpResponse| finalize_response(response, &inputs),
                committed,
            );
            handle_delivery(delivery, &counters, |_state| {});

            let skipped = capture
                .take_warnings()
                .into_iter()
                .filter(|warning| warning.message.contains("`connection`"))
                .count();
            assert_eq!(
                skipped, 1,
                "should warn once about the skipped operator entry for the {label} request"
            );
        }
    }

    #[test]
    fn client_request_values_carry_the_trusted_client_ip_to_client_info_and_geo() {
        let mut settings = test_settings();
        settings.trusted_client_ip = Some(TrustedClientIpConfig {
            ip_header: "x-trusted-client-ip".to_owned(),
            auth_header: "x-trusted-client-auth".to_owned(),
            shared_secret: Redacted::new("fictional-shared-secret-0123456789".to_owned()),
        });
        let trusted = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
        let mut req = FastlyRequest::get("https://publisher.example/article");
        req.set_header("x-trusted-client-ip", "192.0.2.1");
        req.set_header(
            "x-trusted-client-auth",
            "fictional-shared-secret-0123456789",
        );
        assert_ne!(
            req.get_client_ip_addr(),
            Some(trusted),
            "test precondition: the peer IP should differ from the authenticated one"
        );

        let values = ClientRequestValues::read(&mut req, &settings, false);
        let mut extensions = Extensions::new();
        values.prepare(&mut extensions, &settings, &TelemetryQueue::default());

        assert_eq!(
            extensions.get::<ClientInfo>().map(|info| info.client_ip),
            Some(Some(trusted)),
            "should carry the authenticated client IP in ClientInfo"
        );
        assert_eq!(
            extensions.get::<RequestGeo>().map(RequestGeo::client_ip),
            Some(Some(trusted)),
            "should look up geo for the authenticated client IP, not the peer"
        );
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
    fn health_response_reports_git_version_only() {
        let req = FastlyRequest::get("https://example.com/health");

        let response = health_response(&req).expect("should build health response");

        assert_eq!(
            response.get_header_str("x-ts-version"),
            Some(
                trusted_server_core::constants::TS_GIT_VERSION
                    .expect("should compile a git version into the test build")
            ),
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

    fn sample_geo() -> GeoInfo {
        GeoInfo {
            city: "San Francisco".to_owned(),
            country: "US".to_owned(),
            continent: "NorthAmerica".to_owned(),
            latitude: 37.7749,
            longitude: -122.4194,
            metro_code: 807,
            region: Some("CA".to_owned()),
            asn: None,
        }
    }

    #[test]
    fn finalize_sets_ec_cookies_geo_headers_cache_policy_and_counters() {
        let mut settings = test_settings();
        settings.debug.sandbox_metrics_enabled = true;
        settings.response_headers =
            std::iter::once(("x-operator".to_owned(), "kept".to_owned())).collect();
        let geo = sample_geo();
        let counters = counters();
        let inputs = FinalizeInputs {
            settings: &settings,
            geo: Some(&geo),
            counters: &counters,
        };

        // A returning reader withdraws consent with GPC in a US state.
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let request = edgezero_core::http::request_builder()
            .uri("https://www.example.com/article")
            .header("cookie", format!("ts-ec={ec_id}"))
            .header("sec-gpc", "1")
            .body(EdgeBody::empty())
            .expect("should build the reader request");
        let ec_context = trusted_server_core::ec::EcContext::read_from_request_with_geo(
            &settings,
            &request,
            &crate::tinybird::tests::test_services(&ScriptedHttpClient::forbidding()),
            Some(&geo),
        )
        .expect("should read the EC context");
        let mut page = response_builder()
            .header("cache-control", "public, max-age=600")
            .body(EdgeBody::empty())
            .expect("should build the page response");
        page.extensions_mut().insert(EcFinalizeState {
            ec_context,
            use_finalize_kv: false,
            eids_cookie: None,
            sharedid_cookie: None,
            is_real_browser: true,
        });

        let state = finalize_response(&mut page, &inputs);

        assert!(state.is_some(), "should hand the EC state on for pull sync");
        let page_header = |name: &str| {
            page.headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let cookie = page_header("set-cookie").expect("should write the EC cookie");
        assert!(
            cookie.starts_with("ts-ec=;") && cookie.contains("Max-Age=0"),
            "should expire the withdrawn EC cookie, got {cookie}"
        );
        assert_eq!(
            page_header("x-geo-country").as_deref(),
            Some("US"),
            "should apply the request's geo lookup"
        );
        assert_eq!(
            page_header("x-operator").as_deref(),
            Some("kept"),
            "should apply the operator headers"
        );
        assert_eq!(
            page_header("cache-control").as_deref(),
            Some("private, max-age=0"),
            "should keep a cookie-bearing response out of shared caches"
        );

        // An asset response keeps its protected cache policy and, being
        // private and no-store, carries the sandbox counters.
        let mut asset = response_builder()
            .header("cache-control", "public, max-age=600")
            .body(EdgeBody::empty())
            .expect("should build the asset response");
        asset
            .extensions_mut()
            .insert(AssetProxyCachePolicy::NoStorePrivate);

        assert!(
            finalize_response(&mut asset, &inputs).is_none(),
            "should hand no pull-sync inputs on without EC state"
        );
        let asset_header = |name: &str| {
            asset
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
        };
        assert_eq!(
            asset_header("cache-control"),
            Some("no-store, private"),
            "should apply the asset cache policy after the finalize headers"
        );
        assert_eq!(
            asset_header("x-ts-version"),
            Some(
                trusted_server_core::constants::TS_GIT_VERSION
                    .expect("should compile a git version into the test build")
            ),
            "should apply the version header"
        );
        assert_eq!(
            asset_header(sandbox::HEADER_SANDBOX_REQUEST_ID),
            Some("request-example"),
            "should attach the sandbox counters to the private response"
        );
        assert_eq!(
            asset_header(sandbox::HEADER_SANDBOX_ORDINAL),
            Some("2"),
            "should report the request ordinal"
        );
    }

    #[test]
    fn finalize_keeps_counters_off_without_metrics() {
        let settings = test_settings();
        let counters = counters();
        let mut response = response_builder()
            .header("cache-control", "private, no-store")
            .body(EdgeBody::empty())
            .expect("should build response");

        finalize_response(
            &mut response,
            &FinalizeInputs {
                settings: &settings,
                geo: None,
                counters: &counters,
            },
        );

        assert!(
            response
                .headers()
                .get(sandbox::HEADER_SANDBOX_REQUEST_ID)
                .is_none(),
            "counters must stay off responses in the default configuration"
        );
    }

    #[test]
    fn finalize_skips_headers_the_middleware_already_set() {
        let settings = test_settings();
        let counters = counters();
        let mut response = response_builder()
            .header(HEADER_X_TS_FINALIZED, "1")
            .body(EdgeBody::empty())
            .expect("should build response");

        finalize_response(
            &mut response,
            &FinalizeInputs {
                settings: &settings,
                geo: None,
                counters: &counters,
            },
        );

        assert!(
            response.headers().get(HEADER_X_TS_FINALIZED).is_none(),
            "should strip the sentinel"
        );
        assert!(
            response
                .headers()
                .get(trusted_server_core::constants::HEADER_X_GEO_INFO_AVAILABLE)
                .is_none(),
            "should not finalize a middleware-finalized response twice"
        );
    }

    #[test]
    fn sandbox_counters_never_appear_on_cacheable_responses() {
        let counters = counters();
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
        let counters = counters();
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
    fn entry_point_finalize_withholds_geo_from_401() {
        let settings = test_settings();
        let request_geo = GeoInfo {
            city: "Springfield".to_owned(),
            country: "US".to_owned(),
            continent: "NorthAmerica".to_owned(),
            latitude: 39.78,
            longitude: -89.65,
            metro_code: 648,
            region: Some("IL".to_owned()),
            asn: None,
        };
        let mut response = response_builder()
            .status(edgezero_core::http::StatusCode::UNAUTHORIZED)
            .body(EdgeBody::empty())
            .expect("should build response");

        let geo = resolve_geo_for_response(&response, Some(&request_geo));
        apply_finalize_headers(&settings, geo, &mut response);

        assert!(
            geo.is_none(),
            "should withhold the request's geo from a 401"
        );
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
}
