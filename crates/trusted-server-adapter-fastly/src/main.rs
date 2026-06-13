use std::sync::Arc;

use edgezero_adapter_fastly::{into_core_request, FastlyConfigStore};
use edgezero_core::app::Hooks as _;
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::config_store::ConfigStoreHandle;
use edgezero_core::http::{
    header, HeaderValue, Method, Request as HttpRequest, Response as HttpResponse,
};
use error_stack::Report;
use fastly::http::Method as FastlyMethod;
use fastly::{Request as FastlyRequest, Response as FastlyResponse};

use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::AuctionOrchestrator;
use trusted_server_core::auth::enforce_basic_auth;
use trusted_server_core::constants::{COOKIE_SHAREDID, COOKIE_TS_EIDS};
use trusted_server_core::ec::batch_sync::handle_batch_sync;
use trusted_server_core::ec::consent::ec_consent_withdrawn;
use trusted_server_core::ec::device::DeviceSignals;
use trusted_server_core::ec::finalize::ec_finalize_response;
use trusted_server_core::ec::identify::{cors_preflight_identify, handle_identify};
use trusted_server_core::ec::kv::KvIdentityGraph;
use trusted_server_core::ec::pull_sync::{
    build_pull_sync_context, dispatch_pull_sync, PullSyncContext,
};
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::ec::EcContext;
use trusted_server_core::error::{IntoHttpResponse, TrustedServerError};
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::http_util::is_navigation_request;
use trusted_server_core::integrations::{IntegrationRegistry, ProxyDispatchInput};
use trusted_server_core::platform::PlatformGeo as _;
use trusted_server_core::platform::RuntimeServices;
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    handle_publisher_request, handle_tsjs_dynamic, stream_publisher_body,
    OwnedProcessResponseParams, PublisherResponse,
};
use trusted_server_core::request_signing::{
    handle_deactivate_key, handle_rotate_key, handle_trusted_server_discovery,
    handle_verify_signature,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::settings_data::get_settings;

mod app;
mod backend;
mod compat;
mod ec_kv;
mod error;
mod logging;
mod management_api;
mod middleware;
mod platform;
mod rate_limiter;
#[cfg(test)]
mod route_tests;

use crate::app::{build_state, TrustedServerApp};
use crate::ec_kv::FastlyEcKvStore;
use crate::error::to_error_response;
use crate::middleware::{apply_finalize_headers, resolve_geo_for_response, HEADER_X_TS_FINALIZED};
use crate::platform::{build_runtime_services, FastlyPlatformGeo};
use crate::rate_limiter::{FastlyRateLimiter, RATE_COUNTER_NAME};

const TRUSTED_SERVER_CONFIG_STORE: &str = "trusted_server_config";
const EDGEZERO_ENABLED_KEY: &str = "edgezero_enabled";
const EDGEZERO_ROLLOUT_PCT_KEY: &str = "edgezero_rollout_pct";

/// Result of routing a request, distinguishing buffered from streaming publisher responses.
///
/// The streaming arm keeps the publisher body out of WASM heap until it is written directly
/// to the client via [`fastly::Response::stream_to_client`]. All other legacy routes are buffered.
///
/// [`AuthChallenge`](HandlerOutcome::AuthChallenge) marks responses produced by this server's
/// own `enforce_basic_auth` so the geo-lookup gate can distinguish them from origin-forwarded
/// 401s, which should still carry geo headers.
enum HandlerOutcome {
    Buffered(HttpResponse),
    AuthChallenge(HttpResponse),
    Streaming {
        response: HttpResponse,
        body: EdgeBody,
        params: OwnedProcessResponseParams,
    },
}

impl HandlerOutcome {
    #[cfg(test)]
    fn status(&self) -> edgezero_core::http::StatusCode {
        match self {
            HandlerOutcome::Buffered(resp) | HandlerOutcome::AuthChallenge(resp) => resp.status(),
            HandlerOutcome::Streaming { response, .. } => response.status(),
        }
    }
}

/// Returns `true` if the raw config-store value represents an enabled flag.
///
/// Accepted values (after whitespace trimming): `"1"` or `"true"` in any ASCII case.
/// All other values, including the empty string, are treated as disabled.
fn parse_edgezero_flag(value: &str) -> bool {
    let v = value.trim();
    v.eq_ignore_ascii_case("true") || v == "1"
}

/// Parses a rollout percentage string into a value in `0..=100`.
///
/// Accepts only integer strings in the range 0–100 (inclusive) after whitespace
/// trimming. Returns `None` for anything else: non-integer, out-of-range,
/// empty string.
fn parse_rollout_pct(value: &str) -> Option<u8> {
    let n: u16 = value.trim().parse().ok()?;
    if n > 100 {
        return None;
    }
    Some(n as u8)
}

/// Maps an arbitrary string to a deterministic bucket in `0..100`.
///
/// Uses FNV-1a (32-bit variant) to produce a uniform-enough distribution for
/// canary traffic splitting without pulling in any hash crates. The same input
/// always produces the same output across Rust versions because the algorithm
/// is defined here, not delegated to `DefaultHasher`.
fn fnv1a_bucket(key: &str) -> u8 {
    const FNV_OFFSET: u32 = 2_166_136_261;
    const FNV_PRIME: u32 = 16_777_619;
    let mut hash = FNV_OFFSET;
    for byte in key.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    (hash % 100) as u8
}

/// Returns `true` if the given bucket should be routed to the `EdgeZero` path.
///
/// `bucket` must be in `0..100`; `rollout_pct` in `0..=100`.
/// When `rollout_pct = 0` no bucket ever routes to `EdgeZero` (instant rollback).
/// When `rollout_pct = 100` every bucket routes to `EdgeZero` (full cutover).
fn routes_to_edgezero(bucket: u8, rollout_pct: u8) -> bool {
    debug_assert!(bucket < 100, "should be a value produced by fnv1a_bucket");
    debug_assert!(
        rollout_pct <= 100,
        "should be a value produced by read_rollout_pct"
    );
    bucket < rollout_pct
}

/// Opens the shared Fastly Config Store used by both the `EdgeZero` flag read and
/// `EdgeZero` dispatch metadata.
///
/// # Errors
///
/// Returns [`fastly::Error`] if the config store cannot be opened.
fn open_trusted_server_config_store() -> Result<ConfigStoreHandle, fastly::Error> {
    let store = FastlyConfigStore::try_open(TRUSTED_SERVER_CONFIG_STORE)
        .map_err(|e| fastly::Error::msg(format!("failed to open config store: {e}")))?;
    Ok(ConfigStoreHandle::new(Arc::new(store)))
}

/// Reads the `edgezero_enabled` key from the prepared Fastly Config Store
/// handle.
///
/// Returns `Err` on any key-read failure, so callers should use the legacy path
/// as the safe default.
///
/// # Errors
///
/// - [`fastly::Error`] if the key cannot be read.
fn is_edgezero_enabled(config_store: &ConfigStoreHandle) -> Result<bool, fastly::Error> {
    let value = config_store
        .get(EDGEZERO_ENABLED_KEY)
        .map_err(|e| fastly::Error::msg(format!("failed to read edgezero_enabled: {e}")))?;
    Ok(value.as_deref().is_some_and(parse_edgezero_flag))
}

/// Reads `edgezero_rollout_pct` from the config store.
///
/// | Config store state              | Return value | Effect                     |
/// |---------------------------------|--------------|----------------------------|
/// | Key absent                      | `100`        | Full rollout (backward compat) |
/// | Key present, valid 0–100        | parsed value | Partial or full rollout    |
/// | Key present, invalid            | `0`          | All legacy (safe default)  |
/// | Key read error                  | `0`          | All legacy (safe default)  |
fn read_rollout_pct(config_store: &ConfigStoreHandle) -> u8 {
    match config_store.get(EDGEZERO_ROLLOUT_PCT_KEY) {
        Ok(Some(value)) => match parse_rollout_pct(&value) {
            Some(pct) => pct,
            None => {
                log::warn!(
                    "invalid edgezero_rollout_pct value {:?}, defaulting to 0 (legacy path)",
                    value
                );
                0
            }
        },
        Ok(None) => {
            // Fires per-request when the key is absent and edgezero_enabled=true, so
            // emit at debug to avoid a per-request warn flood at production QPS. The
            // setup procedure (set edgezero_rollout_pct = "0" before edgezero_enabled
            // = "true") and the migration runbook are the operational safety net here.
            log::debug!(
                "edgezero_rollout_pct key absent, defaulting to 100 (full rollout — backward compat)"
            );
            100
        }
        Err(e) => {
            log::warn!("failed to read edgezero_rollout_pct: {e}, defaulting to 0 (legacy path)");
            0
        }
    }
}

/// Decides whether a request routes to the `EdgeZero` path for the given rollout state.
///
/// `rollout_pct` must be in `0..=100` (as produced by [`read_rollout_pct`]).
///
/// The degenerate rollout values short-circuit the per-request routing-key
/// allocation and FNV hash, which together cover most of the canary's lifetime:
/// `0` always routes to legacy (full rollback) and `100` always routes to
/// `EdgeZero` (full cutover). Only the partial-rollout path hashes the client IP
/// (or the empty string when absent) into a `0..100` bucket via [`fnv1a_bucket`]
/// and compares it against `rollout_pct` with [`routes_to_edgezero`].
fn should_route_to_edgezero(rollout_pct: u8, client_ip: Option<IpAddr>) -> bool {
    match rollout_pct {
        0 => {
            log::debug!("routing request through legacy path (rollout_pct=0)");
            false
        }
        100 => {
            log::debug!("routing request through EdgeZero path (rollout_pct=100)");
            true
        }
        pct => {
            let routing_key = match client_ip {
                Some(ip) => ip.to_string(),
                None => {
                    log::debug!(
                        "no client IP available, using empty routing key (deterministic bucket 61)"
                    );
                    String::new()
                }
            };
            let bucket = fnv1a_bucket(&routing_key);
            let routed = routes_to_edgezero(bucket, pct);
            log::debug!(
                "routing request through {} path (bucket={bucket}, rollout_pct={pct})",
                if routed { "EdgeZero" } else { "legacy" }
            );
            routed
        }
    }
}

fn health_response(req: &FastlyRequest) -> Option<FastlyResponse> {
    if req.get_method() == FastlyMethod::GET && req.get_path() == "/health" {
        return Some(FastlyResponse::from_status(200).with_body_text_plain("ok"));
    }

    None
}

/// Combined result from `route_request`, bundling the handler outcome with the
/// EC context and cookies needed for post-send finalization and pull sync.
struct RouteResult {
    outcome: HandlerOutcome,
    ec_context: EcContext,
    finalize_kv_graph: Option<KvIdentityGraph>,
    eids_cookie: Option<String>,
    sharedid_cookie: Option<String>,
    is_real_browser: bool,
}

/// Entry point for the Fastly Compute program.
///
/// Uses an undecorated `main()` with `FastlyRequest::from_client()` instead of
/// `#[fastly::main]` so the legacy streaming publisher path can call
/// [`fastly::Response::stream_to_client`] explicitly.
fn main() {
    let req = FastlyRequest::from_client();

    // Health probe bypasses logging, settings, and app construction as a cheap liveness signal.
    if let Some(response) = health_response(&req) {
        response.send_to_client();
        return;
    }

    logging::init_logger();

    let edgezero_config_store = match open_trusted_server_config_store() {
        Ok(config_store) => config_store,
        Err(e) => {
            log::warn!("failed to open EdgeZero config store, falling back to legacy path: {e}");
            legacy_main(req);
            return;
        }
    };

    if !is_edgezero_enabled(&edgezero_config_store).unwrap_or_else(|e| {
        log::warn!("failed to read edgezero_enabled flag, falling back to legacy path: {e}");
        false
    }) {
        log::debug!("routing request through legacy path (edgezero_enabled=false)");
        legacy_main(req);
        return;
    }

    let rollout_pct = read_rollout_pct(&edgezero_config_store);
    let route_to_edgezero = should_route_to_edgezero(rollout_pct, req.get_client_ip_addr());

    if route_to_edgezero {
        edgezero_main(req, edgezero_config_store);
    } else {
        legacy_main(req);
    }
}

/// Handles a request through the `EdgeZero` router path.
fn edgezero_main(mut req: FastlyRequest, config_store: ConfigStoreHandle) {
    // Short-circuit the JA4 debug probe before app construction, mirroring
    // legacy_main. Must run here because TLS/JA4 accessors are only available
    // on FastlyRequest before conversion to edgezero types.
    if req.get_method() == FastlyMethod::GET && req.get_path() == "/_ts/debug/ja4" {
        match get_settings() {
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

    let app = TrustedServerApp::build_app();

    // Strip client-spoofable forwarded headers before handing off to the
    // EdgeZero dispatcher, mirroring the sanitization done in legacy_main.
    compat::sanitize_fastly_forwarded_headers(&mut req);

    // Re-inject a trusted TLS scheme signal after sanitization has stripped any
    // client-sent fastly-ssl header. Setting it from Fastly's native TLS
    // metadata here is authoritative. detect_request_scheme in http_util
    // checks this header so scheme-sensitive logic (publisher URL rewriting,
    // etc.) produces https URLs on HTTPS traffic, matching legacy path parity.
    if req.get_tls_protocol().ok().flatten().is_some()
        || req.get_tls_cipher_openssl_name().ok().flatten().is_some()
    {
        req.set_header("fastly-ssl", "1");
    }

    // Capture client IP before the request is consumed by dispatch.
    let client_ip = req.get_client_ip_addr();

    // Strip any client-supplied x-ts-tls-* headers before injecting the
    // trusted values from the Fastly SDK. Without this, a plain-HTTP request
    // carrying X-TS-TLS-Protocol: TLSv1.3 would sail through and cause
    // detect_request_scheme to return "https", spoofing cookie Secure and
    // URL rewriting. Must run after sanitize_fastly_forwarded_headers.
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

    // Dispatch directly through the EdgeZero router without an intermediate
    // fastly::Response conversion. The standard dispatch helpers
    // (dispatch_with_config_handle, etc.) convert through fastly::Response using
    // set_header, which drops duplicate header values — silently losing multiple
    // Set-Cookie headers from publisher/origin responses.
    //
    // Bypassing to app.router().oneshot() preserves every header value in the
    // http::HeaderMap and skips the logger-reinit that prevents using run_app_*.
    let mut response = {
        match into_core_request(req) {
            Ok(mut core_req) => {
                core_req.extensions_mut().insert(config_store);
                futures::executor::block_on(app.router().oneshot(core_req))
            }
            Err(e) => {
                log::error!("EdgeZero request conversion failed: {e}");
                FastlyResponse::from_status(fastly::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_body_text_plain("Internal Server Error")
                    .send_to_client();
                return;
            }
        }
    };

    // Pop the EC finalize state that route handlers thread out via response
    // extensions. Must happen before the fastly conversion, which drops
    // extensions.
    let ec_state = response
        .extensions_mut()
        .remove::<crate::app::EcFinalizeState>();

    if !take_finalize_sentinel(&mut response) {
        // Apply finalize headers at the entry point so that router-level
        // 405/404 responses for unregistered HTTP methods (e.g. TRACE, WebDAV
        // verbs) carry TS/geo headers. Middleware-finalized responses are
        // skipped here to avoid a second settings read and geo lookup on the
        // normal registered-route path.
        match get_settings() {
            Ok(settings) => {
                let geo_info = resolve_geo_for_response(&response, client_ip, |client_ip| {
                    FastlyPlatformGeo.lookup(client_ip).unwrap_or_else(|e| {
                        log::warn!("entry-point geo lookup failed: {e}");
                        None
                    })
                });
                apply_finalize_headers(&settings, geo_info.as_ref(), &mut response);
            }
            Err(e) => {
                log::warn!("entry-point finalize skipped: failed to reload settings: {e:?}");
            }
        }
    }

    // EC response lifecycle, mirroring legacy_main: finalize EC cookies and
    // request headers on the response, send it, then run pull sync for
    // recognized browsers. When settings or the partner registry cannot be
    // loaded the response is sent without EC finalization rather than
    // dropped.
    if let Some(ec_state) = ec_state {
        match get_settings() {
            Ok(settings) => match PartnerRegistry::from_config(&settings.ec.partners) {
                Ok(partner_registry) => {
                    // KvIdentityGraph cannot ride in response extensions
                    // (non-Sync dyn EcKvStore), so rebuild it from settings
                    // when the handler enabled the KV write path.
                    let finalize_kv_graph = if ec_state.use_finalize_kv {
                        maybe_identity_graph(&settings)
                    } else {
                        None
                    };
                    ec_finalize_response(
                        &settings,
                        &ec_state.ec_context,
                        finalize_kv_graph.as_ref(),
                        &partner_registry,
                        ec_state.eids_cookie.as_deref(),
                        ec_state.sharedid_cookie.as_deref(),
                        &mut response,
                    );
                    compat::to_fastly_response(response).send_to_client();

                    if ec_state.is_real_browser {
                        if let Some(context) = build_pull_sync_context(&ec_state.ec_context) {
                            run_pull_sync_after_send(
                                &settings,
                                &partner_registry,
                                &context,
                                &ec_state.services,
                            );
                        }
                    }
                    return;
                }
                Err(e) => {
                    log::error!(
                        "EdgeZero EC finalize skipped: failed to build partner registry: {e:?}"
                    );
                }
            },
            Err(e) => {
                log::warn!("EdgeZero EC finalize skipped: failed to reload settings: {e:?}");
            }
        }
    }

    compat::to_fastly_response(response).send_to_client();
}

fn take_finalize_sentinel(response: &mut HttpResponse) -> bool {
    response
        .headers_mut()
        .remove(HEADER_X_TS_FINALIZED)
        .is_some()
}

/// Handles a request using the original Fastly-native entry point.
///
/// Preserves identical semantics to the pre-PR14 `main()`, with one
/// relocation: `GET /health` is short-circuited in [`main`] before the flag
/// dispatch, so it never reaches this function. The pre-PR14 entry point
/// answered `/health` with the same `200 ok` before settings loading and
/// routing; the only difference is that the probe now also skips logger
/// initialization. Called whenever
/// the `EdgeZero` flag is disabled or cannot be read/parsed as enabled — that
/// includes config-store open failures, key-read errors, missing keys, and
/// any value other than the accepted `"true"` / `"1"` forms.
///
/// The thin fastly↔http conversion layer (via `compat::from_fastly_request` /
/// `compat::to_fastly_response`) lives here in the adapter crate.
// TODO: delete after Phase 5 EdgeZero cutover — see issue #495
fn legacy_main(mut req: FastlyRequest) {
    let state = match build_state() {
        Ok(state) => state,
        Err(e) => {
            log::error!("Failed to build application state: {:?}", e);
            to_error_response(&e).send_to_client();
            return;
        }
    };
    // lgtm[rust/cleartext-logging]
    // `Settings` uses `Redacted<T>` for secrets, so this debug dump is redacted.
    log::debug!("Settings {:?}", state.settings);

    // Short-circuit the ja4 debug probe before finalize_response so that
    // Cache-Control: no-store, private cannot be replaced by operator [response_headers].
    if req.get_method() == FastlyMethod::GET && req.get_path() == "/_ts/debug/ja4" {
        if state.settings.debug.ja4_endpoint_enabled {
            build_ja4_debug_response(&req).send_to_client();
        } else {
            FastlyResponse::from_status(fastly::http::StatusCode::NOT_FOUND).send_to_client();
        }
        return;
    }

    let partner_registry = match PartnerRegistry::from_config(&state.settings.ec.partners) {
        Ok(registry) => registry,
        Err(e) => {
            log::error!("Failed to build partner registry: {:?}", e);
            to_error_response(&e).send_to_client();
            return;
        }
    };

    // Strip client-spoofable forwarded headers at the edge before building
    // any request-derived context or converting to the core HTTP types.
    compat::sanitize_fastly_forwarded_headers(&mut req);

    let device_signals = derive_device_signals(&req);
    let runtime_services =
        build_runtime_services(&req, std::sync::Arc::clone(&state.default_kv_store));
    let http_req = compat::from_fastly_request(req);

    let route_result = futures::executor::block_on(route_request(
        &state.settings,
        &state.orchestrator,
        &state.registry,
        &partner_registry,
        &runtime_services,
        http_req,
        device_signals,
    ))
    .unwrap_or_else(|e| RouteResult {
        outcome: HandlerOutcome::Buffered(http_error_response(&e)),
        ec_context: EcContext::default(),
        finalize_kv_graph: None,
        eids_cookie: None,
        sharedid_cookie: None,
        is_real_browser: false,
    });

    let RouteResult {
        outcome,
        ec_context,
        finalize_kv_graph,
        eids_cookie,
        sharedid_cookie,
        is_real_browser,
    } = route_result;

    // Skip geo lookup for our own auth challenges: avoids exposing geo headers to
    // unauthenticated callers.  Origin-forwarded 401s are not AuthChallenge and
    // do receive geo headers — the client already reached the origin anyway.
    let geo_info = if matches!(outcome, HandlerOutcome::AuthChallenge(_)) {
        None
    } else {
        runtime_services
            .geo()
            .lookup(runtime_services.client_info().client_ip)
            .unwrap_or_else(|e| {
                log::warn!("geo lookup failed: {e}");
                None
            })
    };

    match outcome {
        HandlerOutcome::Buffered(mut response) | HandlerOutcome::AuthChallenge(mut response) => {
            finalize_response(&state.settings, geo_info.as_ref(), &mut response);
            ec_finalize_response(
                &state.settings,
                &ec_context,
                finalize_kv_graph.as_ref(),
                &partner_registry,
                eids_cookie.as_deref(),
                sharedid_cookie.as_deref(),
                &mut response,
            );
            compat::to_fastly_response(response).send_to_client();

            if is_real_browser {
                if let Some(context) = build_pull_sync_context(&ec_context) {
                    run_pull_sync_after_send(
                        &state.settings,
                        &partner_registry,
                        &context,
                        &runtime_services,
                    );
                }
            }
        }
        HandlerOutcome::Streaming {
            mut response,
            body,
            params,
        } => {
            finalize_response(&state.settings, geo_info.as_ref(), &mut response);
            ec_finalize_response(
                &state.settings,
                &ec_context,
                finalize_kv_graph.as_ref(),
                &partner_registry,
                eids_cookie.as_deref(),
                sharedid_cookie.as_deref(),
                &mut response,
            );
            let fastly_resp = compat::to_fastly_response_skeleton(response);
            let mut streaming_body = fastly_resp.stream_to_client();
            let mut stream_succeeded = false;
            match stream_publisher_body(
                body,
                &mut streaming_body,
                &params,
                &state.settings,
                &state.registry,
            ) {
                Ok(()) => {
                    if let Err(e) = streaming_body.finish() {
                        log::error!("failed to finish streaming body: {e}");
                    } else {
                        stream_succeeded = true;
                    }
                }
                Err(e) => {
                    log::error!("streaming processing failed: {e:?}");
                    // Headers already committed. Drop the body so the client sees a
                    // truncated response (EOF mid-stream) — standard proxy behavior.
                    drop(streaming_body);
                }
            }

            if is_real_browser && stream_succeeded {
                if let Some(context) = build_pull_sync_context(&ec_context) {
                    run_pull_sync_after_send(
                        &state.settings,
                        &partner_registry,
                        &context,
                        &runtime_services,
                    );
                }
            }
        }
    }
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

async fn route_request(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    integration_registry: &IntegrationRegistry,
    partner_registry: &PartnerRegistry,
    runtime_services: &RuntimeServices,
    req: HttpRequest,
    device_signals: DeviceSignals,
) -> Result<RouteResult, Report<TrustedServerError>> {
    let is_real_browser = device_signals.looks_like_browser();

    if !is_real_browser {
        log::info!(
            "Bot gate: blocking EC operations (ja4={:?}, platform={:?}, is_mobile={})",
            device_signals.ja4_class,
            device_signals.platform_class,
            device_signals.is_mobile,
        );
    }

    // Extract the Prebid EIDs and SharedID cookies before routing.
    let eids_cookie = extract_cookie_value(&req, COOKIE_TS_EIDS);
    let sharedid_cookie = extract_cookie_value(&req, COOKIE_SHAREDID);

    // Extract geo info.
    let geo_info = runtime_services
        .geo()
        .lookup(runtime_services.client_info().client_ip)
        .unwrap_or_else(|e| {
            log::warn!("geo lookup failed during routing: {e}");
            None
        });

    // S2S batch sync — uses Bearer auth (not EC cookies), so skip EC
    // context creation and the EC finalize middleware entirely.
    if req.method() == Method::POST && req.uri().path() == "/_ts/api/v1/batch-sync" {
        match enforce_basic_auth(settings, &req) {
            Ok(Some(response)) => {
                return Ok(RouteResult {
                    outcome: HandlerOutcome::AuthChallenge(response),
                    ec_context: EcContext::default(),
                    finalize_kv_graph: None,
                    eids_cookie,
                    sharedid_cookie,
                    is_real_browser,
                });
            }
            Ok(None) => {}
            Err(e) => return Err(e),
        }
        let result = require_identity_graph(settings).and_then(|kv| {
            let limiter = FastlyRateLimiter::new(RATE_COUNTER_NAME);
            handle_batch_sync(&kv, partner_registry, &limiter, req)
        });
        let outcome = match result {
            Ok(resp) => HandlerOutcome::Buffered(resp),
            Err(e) => HandlerOutcome::Buffered(http_error_response(&e)),
        };
        return Ok(RouteResult {
            outcome,
            ec_context: EcContext::default(),
            finalize_kv_graph: None,
            eids_cookie,
            sharedid_cookie,
            is_real_browser,
        });
    }

    // Build EC context using the fastly request reference (headers/method/URI).
    let mut ec_context = match EcContext::read_from_request_with_geo(
        settings,
        &req,
        runtime_services,
        geo_info.as_ref(),
    ) {
        Ok(context) => context,
        Err(err) => {
            return Ok(RouteResult {
                outcome: HandlerOutcome::Buffered(http_error_response(&err)),
                ec_context: EcContext::default(),
                finalize_kv_graph: None,
                eids_cookie,
                sharedid_cookie,
                is_real_browser,
            });
        }
    };

    // Pass device signals to EcContext so they are stored on new entries.
    ec_context.set_device_signals(device_signals);

    // Bot gate: suppress KV-backed EC writes for unrecognized clients, except
    // consent withdrawals. Revocations need the KV graph so tombstones remain
    // authoritative even for privacy-extension-heavy clients that do not look
    // like known browsers.
    // Build the KV identity graph once. The write-path (finalize_kv_graph) is
    // also given to bots when they signal consent withdrawal so tombstones are
    // authoritative even for privacy-extension-heavy clients.
    let kv_graph = maybe_identity_graph(settings);
    let finalize_kv_graph = if is_real_browser || ec_consent_withdrawn(ec_context.consent()) {
        kv_graph.clone()
    } else {
        None
    };
    let kv_graph = if is_real_browser { kv_graph } else { None };

    // `get_settings()` should already have rejected invalid handler regexes.
    // Keep this fallback so manually-constructed or otherwise unprepared
    // settings still become an error response instead of panicking.
    match enforce_basic_auth(settings, &req) {
        Ok(Some(response)) => {
            return Ok(RouteResult {
                outcome: HandlerOutcome::AuthChallenge(response),
                ec_context,
                finalize_kv_graph,
                eids_cookie,
                sharedid_cookie,
                is_real_browser,
            });
        }
        Ok(None) => {}
        Err(e) => return Err(e),
    }

    // Get path and method for routing.
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    // Match known routes and handle them
    let (result, organic_route) = match (method, path.as_str()) {
        // Serve the tsjs library
        (Method::GET, path) if path.starts_with("/static/tsjs=") => {
            (handle_tsjs_dynamic(&req, integration_registry), false)
        }

        // Discovery endpoint for trusted-server capabilities and JWKS
        (Method::GET, "/.well-known/trusted-server.json") => (
            handle_trusted_server_discovery(settings, runtime_services, req),
            false,
        ),

        // Signature verification endpoint
        (Method::POST, "/verify-signature") => (
            handle_verify_signature(settings, runtime_services, req),
            false,
        ),

        // Admin endpoints
        // Keep in sync with Settings::ADMIN_ENDPOINTS in crates/trusted-server-core/src/settings.rs
        (Method::POST, "/admin/keys/rotate") | (Method::POST, "/_ts/admin/keys/rotate") => {
            (handle_rotate_key(settings, runtime_services, req), false)
        }
        (Method::POST, "/admin/keys/deactivate") | (Method::POST, "/_ts/admin/keys/deactivate") => {
            (
                handle_deactivate_key(settings, runtime_services, req),
                false,
            )
        }
        (Method::GET, "/_ts/api/v1/identify") => {
            let outcome = require_identity_graph(settings)
                .and_then(|kv| handle_identify(settings, &kv, partner_registry, &req, &ec_context));
            (outcome, false)
        }
        (Method::OPTIONS, "/_ts/api/v1/identify") => {
            let outcome = cors_preflight_identify(settings, &req);
            (outcome, false)
        }

        // Unified auction endpoint.
        (Method::POST, "/auction") => {
            let registry_ref = if partner_registry.is_empty() {
                None
            } else {
                Some(partner_registry)
            };
            (
                handle_auction(
                    settings,
                    orchestrator,
                    kv_graph.as_ref(),
                    registry_ref,
                    &ec_context,
                    runtime_services,
                    req,
                )
                .await,
                false,
            )
        }

        // First-party proxy/click/sign/rebuild endpoints
        (Method::GET, "/first-party/proxy") => (
            handle_first_party_proxy(settings, runtime_services, req).await,
            false,
        ),
        (Method::GET, "/first-party/click") => (
            handle_first_party_click(settings, runtime_services, req).await,
            false,
        ),
        (Method::GET, "/first-party/sign") | (Method::POST, "/first-party/sign") => (
            handle_first_party_proxy_sign(settings, runtime_services, req).await,
            false,
        ),
        (Method::POST, "/first-party/proxy-rebuild") => (
            handle_first_party_proxy_rebuild(settings, runtime_services, req).await,
            false,
        ),
        (m, path) if integration_registry.has_route(&m, path) => {
            let result = integration_registry
                .handle_proxy(ProxyDispatchInput {
                    method: &m,
                    path,
                    settings,
                    kv: kv_graph.as_ref(),
                    ec_context: &mut ec_context,
                    services: runtime_services,
                    req,
                })
                .await
                .unwrap_or_else(|| {
                    Err(Report::new(TrustedServerError::BadRequest {
                        message: format!("Unknown integration route: {path}"),
                    }))
                });
            (result, true)
        }

        // No known route matched, proxy to publisher origin as fallback.
        _ => {
            log::info!(
                "No known route matched for path: {}, proxying to publisher origin",
                path
            );

            // Generate EC ID if needed — mirrors the integration proxy path in registry.rs.
            // Only for document navigations by recognised browsers; subresource requests
            // may lack consent signals such as Sec-GPC.
            if is_real_browser && is_navigation_request(&req) {
                if let Err(err) = ec_context.generate_if_needed(settings, kv_graph.as_ref()) {
                    log::warn!("EC generation failed for publisher proxy: {err:?}");
                }
            }

            match handle_publisher_request(settings, integration_registry, runtime_services, req)
                .await
            {
                Ok(PublisherResponse::Stream {
                    response,
                    body,
                    params,
                }) => {
                    return Ok(RouteResult {
                        outcome: HandlerOutcome::Streaming {
                            response,
                            body,
                            params,
                        },
                        ec_context,
                        finalize_kv_graph,
                        eids_cookie,
                        sharedid_cookie,
                        is_real_browser,
                    });
                }
                Ok(PublisherResponse::PassThrough { mut response, body }) => {
                    *response.body_mut() = body;
                    (Ok(response), true)
                }
                Ok(PublisherResponse::Buffered(response)) => (Ok(response), true),
                Err(e) => {
                    log::error!("Failed to proxy to publisher origin: {:?}", e);
                    (Err(e), true)
                }
            }
        }
    };

    let _ = organic_route;

    let outcome = result
        .map(HandlerOutcome::Buffered)
        .unwrap_or_else(|e| HandlerOutcome::Buffered(http_error_response(&e)));

    Ok(RouteResult {
        outcome,
        ec_context,
        finalize_kv_graph,
        eids_cookie,
        sharedid_cookie,
        is_real_browser,
    })
}

pub(crate) fn maybe_identity_graph(settings: &Settings) -> Option<KvIdentityGraph> {
    settings
        .ec
        .ec_store
        .as_ref()
        .map(|store_name| KvIdentityGraph::new(FastlyEcKvStore::new(store_name)))
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

/// Applies all standard response headers: geo, version, staging, and configured headers.
///
/// Called from every response path (including auth early-returns) so that all
/// outgoing responses carry a consistent set of Trusted Server headers.
///
/// Header precedence (last write wins): geo headers are set first, then
/// version/staging, then operator-configured `settings.response_headers`.
/// This means operators can intentionally override any managed header.
fn finalize_response(settings: &Settings, geo_info: Option<&GeoInfo>, response: &mut HttpResponse) {
    apply_finalize_headers(settings, geo_info, response);
}

fn http_error_response(report: &Report<TrustedServerError>) -> HttpResponse {
    let root_error = report.current_context();
    log::error!("Error occurred: {:?}", report);

    let mut response =
        HttpResponse::new(EdgeBody::from(format!("{}\n", root_error.user_message())));
    *response.status_mut() = root_error.status_code();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
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
        if let Some((key, value)) = pair.split_once('=') {
            if key.trim() == name {
                return Some(value.trim().to_owned());
            }
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
    use edgezero_core::http::response_builder;
    use fastly::mime;

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

    #[test]
    fn parses_true_flag_values() {
        assert!(parse_edgezero_flag("true"), "should parse 'true'");
        assert!(parse_edgezero_flag("1"), "should parse '1'");
        assert!(parse_edgezero_flag("  true  "), "should trim whitespace");
        assert!(
            parse_edgezero_flag("  1  "),
            "should trim whitespace around '1'"
        );
        assert!(parse_edgezero_flag("TRUE"), "should parse uppercase 'TRUE'");
        assert!(
            parse_edgezero_flag("True"),
            "should parse mixed-case 'True'"
        );
    }

    #[test]
    fn rejects_non_true_flag_values() {
        assert!(!parse_edgezero_flag("false"), "should not parse 'false'");
        assert!(!parse_edgezero_flag(""), "should not parse empty string");
        assert!(
            !parse_edgezero_flag("  "),
            "should not parse whitespace-only"
        );
        assert!(!parse_edgezero_flag("yes"), "should not parse 'yes'");
    }

    // ---------------------------------------------------------------------------
    // parse_rollout_pct
    // ---------------------------------------------------------------------------

    #[test]
    fn parses_valid_rollout_percentages() {
        assert_eq!(parse_rollout_pct("0"), Some(0), "should parse '0'");
        assert_eq!(parse_rollout_pct("1"), Some(1), "should parse '1'");
        assert_eq!(parse_rollout_pct("50"), Some(50), "should parse '50'");
        assert_eq!(parse_rollout_pct("100"), Some(100), "should parse '100'");
        assert_eq!(
            parse_rollout_pct("  50  "),
            Some(50),
            "should trim whitespace"
        );
    }

    #[test]
    fn rejects_invalid_rollout_percentages() {
        assert_eq!(
            parse_rollout_pct("101"),
            None,
            "should reject values above 100"
        );
        assert_eq!(parse_rollout_pct(""), None, "should reject empty string");
        assert_eq!(parse_rollout_pct("abc"), None, "should reject non-integer");
        assert_eq!(
            parse_rollout_pct("-1"),
            None,
            "should reject negative value"
        );
        assert_eq!(
            parse_rollout_pct("1.5"),
            None,
            "should reject decimal value"
        );
    }

    // ---------------------------------------------------------------------------
    // fnv1a_bucket
    // ---------------------------------------------------------------------------

    #[test]
    fn bucket_is_in_range_0_to_99() {
        for key in &["1.2.3.4", "255.255.255.255", "::1", "", "unknown"] {
            let b = fnv1a_bucket(key);
            assert!(b < 100, "bucket must be 0..100 for key {key:?}, got {b}");
        }
    }

    #[test]
    fn bucket_is_deterministic() {
        let key = "192.168.1.1";
        assert_eq!(
            fnv1a_bucket(key),
            fnv1a_bucket(key),
            "same key must produce the same bucket"
        );
    }

    #[test]
    fn bucket_matches_known_fnv1a_vector() {
        // FNV-1a 32-bit: XOR-then-multiply. Verified against reference implementation.
        assert_eq!(
            fnv1a_bucket("1.2.3.4"),
            85,
            "should match pinned FNV-1a vector"
        );
        assert_eq!(
            fnv1a_bucket(""),
            61,
            "should match pinned FNV-1a vector for empty key"
        );
    }

    #[test]
    fn bucket_distributes_across_range() {
        // Smoke-test that fnv1a_bucket produces a spread of values (not a constant).
        // 256 distinct IP-like keys must produce at least 50 unique buckets.
        let buckets: std::collections::HashSet<u8> = (0u16..=255)
            .map(|i| fnv1a_bucket(&format!("10.0.0.{i}")))
            .collect();
        assert!(
            buckets.len() > 50,
            "fnv1a_bucket should distribute across buckets; got only {} unique values in 256 keys",
            buckets.len()
        );
    }

    #[test]
    fn empty_key_bucket_is_valid() {
        let b = fnv1a_bucket("");
        assert!(
            b < 100,
            "empty key must still produce a valid bucket, got {b}"
        );
    }

    // ---------------------------------------------------------------------------
    // routes_to_edgezero
    // ---------------------------------------------------------------------------

    #[test]
    fn rollout_zero_routes_all_to_legacy() {
        for bucket in 0u8..100 {
            assert!(
                !routes_to_edgezero(bucket, 0),
                "pct=0 should route all to legacy, bucket={bucket}"
            );
        }
    }

    #[test]
    fn rollout_hundred_routes_all_to_edgezero() {
        for bucket in 0u8..100 {
            assert!(
                routes_to_edgezero(bucket, 100),
                "pct=100 should route all to EdgeZero, bucket={bucket}"
            );
        }
    }

    #[test]
    fn rollout_fifty_routes_exactly_half_of_bucket_space() {
        let edgezero_count = (0u8..100).filter(|&b| routes_to_edgezero(b, 50)).count();
        assert_eq!(
            edgezero_count, 50,
            "pct=50 should route exactly 50 out of 100 buckets to EdgeZero"
        );
    }

    #[test]
    fn rollout_one_routes_exactly_one_bucket() {
        let edgezero_count = (0u8..100).filter(|&b| routes_to_edgezero(b, 1)).count();
        assert_eq!(
            edgezero_count, 1,
            "pct=1 should route exactly 1 out of 100 buckets to EdgeZero"
        );
    }

    // ---------------------------------------------------------------------------
    // should_route_to_edgezero — entry-point dispatch matrix
    // ---------------------------------------------------------------------------

    #[test]
    fn should_route_zero_pct_always_routes_to_legacy() {
        let ip: IpAddr = "1.2.3.4".parse().expect("should parse IP");
        assert!(
            !should_route_to_edgezero(0, Some(ip)),
            "rollout_pct=0 should route to legacy regardless of client IP"
        );
        assert!(
            !should_route_to_edgezero(0, None),
            "rollout_pct=0 should route to legacy when client IP is absent"
        );
    }

    #[test]
    fn should_route_hundred_pct_always_routes_to_edgezero() {
        let ip: IpAddr = "1.2.3.4".parse().expect("should parse IP");
        assert!(
            should_route_to_edgezero(100, Some(ip)),
            "rollout_pct=100 should route to EdgeZero regardless of client IP"
        );
        assert!(
            should_route_to_edgezero(100, None),
            "rollout_pct=100 should route to EdgeZero when client IP is absent"
        );
    }

    #[test]
    fn should_route_partial_buckets_client_ip() {
        // "1.2.3.4" hashes to bucket 85 (pinned FNV-1a vector).
        let ip: IpAddr = "1.2.3.4".parse().expect("should parse IP");
        assert!(
            should_route_to_edgezero(86, Some(ip)),
            "bucket 85 < 86 should route to EdgeZero (hit)"
        );
        assert!(
            !should_route_to_edgezero(85, Some(ip)),
            "bucket 85 is not < 85 should route to legacy (miss)"
        );
    }

    #[test]
    fn should_route_partial_absent_ip_uses_empty_key_bucket() {
        // The empty routing key hashes to bucket 61 (pinned FNV-1a vector).
        assert!(
            should_route_to_edgezero(62, None),
            "bucket 61 < 62 should route to EdgeZero (hit)"
        );
        assert!(
            !should_route_to_edgezero(61, None),
            "bucket 61 is not < 61 should route to legacy (miss)"
        );
    }

    // ---------------------------------------------------------------------------
    // read_rollout_pct — safety-critical config-store branches
    // ---------------------------------------------------------------------------

    // Canned config-store response so the safety-critical defaults can be pinned
    // without a live Fastly Config Store. `ConfigStoreError` is not `Clone`, so the
    // error case is built fresh inside `get` rather than stored.
    enum StubResponse {
        Value(String),
        Absent,
        Unavailable,
    }

    struct TestConfigStore {
        response: StubResponse,
    }

    impl edgezero_core::config_store::ConfigStore for TestConfigStore {
        fn get(
            &self,
            _key: &str,
        ) -> Result<Option<String>, edgezero_core::config_store::ConfigStoreError> {
            match &self.response {
                StubResponse::Value(v) => Ok(Some(v.clone())),
                StubResponse::Absent => Ok(None),
                StubResponse::Unavailable => Err(
                    edgezero_core::config_store::ConfigStoreError::unavailable("boom"),
                ),
            }
        }
    }

    fn rollout_handle(response: StubResponse) -> ConfigStoreHandle {
        ConfigStoreHandle::new(Arc::new(TestConfigStore { response }))
    }

    #[test]
    fn read_rollout_pct_absent_defaults_to_full_rollout() {
        assert_eq!(
            read_rollout_pct(&rollout_handle(StubResponse::Absent)),
            100,
            "absent key should default to 100 (backward compat)"
        );
    }

    #[test]
    fn read_rollout_pct_valid_value_is_parsed() {
        assert_eq!(
            read_rollout_pct(&rollout_handle(StubResponse::Value("42".into()))),
            42,
            "a valid in-range value should be returned verbatim"
        );
    }

    #[test]
    fn read_rollout_pct_invalid_value_defaults_to_zero() {
        assert_eq!(
            read_rollout_pct(&rollout_handle(StubResponse::Value("abc".into()))),
            0,
            "an unparseable value should fail safe to 0 (legacy)"
        );
    }

    #[test]
    fn read_rollout_pct_out_of_range_defaults_to_zero() {
        assert_eq!(
            read_rollout_pct(&rollout_handle(StubResponse::Value("101".into()))),
            0,
            "an out-of-range value should fail safe to 0 (legacy)"
        );
    }

    #[test]
    fn read_rollout_pct_read_error_defaults_to_zero() {
        assert_eq!(
            read_rollout_pct(&rollout_handle(StubResponse::Unavailable)),
            0,
            "a config-store read error should fail safe to 0 (legacy)"
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
    #[allow(clippy::panic)]
    fn entry_point_finalize_skips_geo_lookup_for_401() {
        let settings = test_settings();
        let mut response = response_builder()
            .status(edgezero_core::http::StatusCode::UNAUTHORIZED)
            .body(EdgeBody::empty())
            .expect("should build response");

        let geo_info = resolve_geo_for_response(&response, None, |_| {
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
            "should include H2 fingerprint fallback"
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
