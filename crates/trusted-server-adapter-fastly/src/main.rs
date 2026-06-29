use std::sync::Arc;

use edgezero_adapter_fastly::config_store::FastlyConfigStore;
use edgezero_adapter_fastly::request::into_core_request;
use edgezero_core::app::Hooks as _;
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::config_store::ConfigStoreHandle;
use edgezero_core::http::{
    header, HeaderValue, Method, Request as HttpRequest, Response as HttpResponse, StatusCode,
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
use trusted_server_core::ec::rate_limiter::{FastlyRateLimiter, RATE_COUNTER_NAME};
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::ec::EcContext;
use trusted_server_core::error::{IntoHttpResponse, TrustedServerError};
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::http_util::is_navigation_request;
use trusted_server_core::integrations::{
    IntegrationRegistry, ProxyDispatchInput, RequestFilterEffects, RequestFilterRegistryInput,
    RequestFilterRegistryOutcome,
};
use trusted_server_core::platform::PlatformGeo as _;
use trusted_server_core::platform::RuntimeServices;
use trusted_server_core::proxy::{
    handle_asset_proxy_request, handle_first_party_click, handle_first_party_proxy,
    handle_first_party_proxy_rebuild, handle_first_party_proxy_sign, stream_asset_body,
    AssetProxyCachePolicy,
};
use trusted_server_core::publisher::{
    handle_page_bids, handle_publisher_request, handle_tsjs_dynamic, stream_publisher_body,
    stream_publisher_body_async, BoundedWriter, OwnedProcessResponseParams, PublisherResponse,
};
use trusted_server_core::request_signing::{
    handle_deactivate_key, handle_rotate_key, handle_trusted_server_discovery,
    handle_verify_signature,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::settings_data::get_settings;
use trusted_server_core::tester_cookie::{handle_clear_tester, handle_set_tester};

mod app;
mod backend;
mod compat;
mod error;
mod logging;
mod management_api;
mod middleware;
mod platform;
#[cfg(test)]
mod route_tests;
mod tinybird;

use crate::app::{build_state, TrustedServerApp};
use crate::error::to_error_response;
use crate::middleware::{
    apply_finalize_headers, cache_control_is_uncacheable, resolve_geo_for_response,
    FASTLY_SURROGATE_CONTROL_HEADER, HEADER_X_TS_FINALIZED, PRIVATE_CACHE_CONTROL_VALUE,
    SURROGATE_CONTROL_HEADER,
};
use crate::platform::{build_runtime_services, client_info_from_request, FastlyPlatformGeo};

const TRUSTED_SERVER_CONFIG_STORE: &str = "trusted_server_config";
const EDGEZERO_ENABLED_KEY: &str = "edgezero_enabled";

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
        params: Box<OwnedProcessResponseParams>,
    },
    AssetStreaming {
        response: HttpResponse,
        body: EdgeBody,
    },
}

impl HandlerOutcome {
    #[cfg(test)]
    fn status(&self) -> edgezero_core::http::StatusCode {
        match self {
            HandlerOutcome::Buffered(resp) | HandlerOutcome::AuthChallenge(resp) => resp.status(),
            HandlerOutcome::Streaming { response, .. }
            | HandlerOutcome::AssetStreaming { response, .. } => response.status(),
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

fn edgezero_can_handle_settings(settings: &Settings) -> bool {
    settings
        .creative_opportunities
        .as_ref()
        .is_none_or(|creative_opportunities| creative_opportunities.slot.is_empty())
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
    should_finalize_ec: bool,
    asset_cache_policy: AssetProxyCachePolicy,
    request_filter_effects: RequestFilterEffects,
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

    if is_edgezero_enabled(&edgezero_config_store).unwrap_or_else(|e| {
        log::warn!("failed to read edgezero_enabled flag, falling back to legacy path: {e}");
        false
    }) {
        match get_settings() {
            Ok(settings) if edgezero_can_handle_settings(&settings) => {
                log::debug!("routing request through EdgeZero path");
                edgezero_main(req, edgezero_config_store);
            }
            Ok(_) => {
                log::warn!(
                    "EdgeZero path does not yet support configured creative_opportunity slots; routing through legacy path"
                );
                legacy_main(req);
            }
            Err(e) => {
                log::warn!(
                    "failed to load settings for EdgeZero compatibility check, falling back to legacy path: {e:?}"
                );
                legacy_main(req);
            }
        }
    } else {
        log::debug!("routing request through legacy path");
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

    // Capture the full ClientInfo (TLS protocol/cipher, JA4, H2 fingerprint, and
    // server hostname/region) from the original FastlyRequest before conversion.
    // These accessors only return real values on the client request; the
    // reconstructed EdgeZero request cannot expose them. Stored in the request
    // extensions so `build_per_request_services` reads the authoritative metadata
    // that integration bot protection (e.g. DataDome) serializes, instead of
    // defaulting those fields to empty as the EdgeZero context alone would.
    let client_info = client_info_from_request(&req);

    // Derive device signals from the original FastlyRequest before conversion.
    // Fastly's `get_tls_ja4()` and `get_client_h2_fingerprint()` accessors only
    // return real values on the client request; a synthetic request rebuilt from
    // EdgeZero HTTP types cannot expose them, which would strip the JA4/H2 class
    // the EC bot gate needs and misclassify real browsers as bots. Stored in the
    // request extensions so `build_ec_request_state` reads the authoritative
    // signals instead of re-deriving from the reconstructed request.
    let device_signals = derive_device_signals(&req);

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
                core_req.extensions_mut().insert(device_signals);
                core_req.extensions_mut().insert(client_info);
                match futures::executor::block_on(app.router().oneshot(core_req)) {
                    Ok(resp) => resp,
                    Err(e) => {
                        log::error!("EdgeZero dispatch failed: {e}");
                        FastlyResponse::from_status(
                            fastly::http::StatusCode::INTERNAL_SERVER_ERROR,
                        )
                        .with_body_text_plain("Internal Server Error")
                        .send_to_client();
                        return;
                    }
                }
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

    // Pop the asset cache policy threaded out by the asset-route fallback. Must
    // happen before the fastly conversion, which drops extensions. Reapplied
    // after finalization below so protected directives (e.g. no-store on asset
    // errors) survive operator `response_headers`, mirroring legacy_main's
    // asset_cache_policy.apply_after_route_finalization.
    let asset_cache_policy = response.extensions_mut().remove::<AssetProxyCachePolicy>();

    // Pop the integration request-filter response effects (e.g. DataDome
    // challenge/allow headers) threaded out by the dispatch path. Applied to the
    // response after EC finalization and before send, mirroring legacy_main's
    // `request_filter_effects` application. Must happen before the fastly
    // conversion, which drops extensions.
    let request_filter_effects = response.extensions_mut().remove::<RequestFilterEffects>();

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

    // Reapply protected asset cache directives after finalization, mirroring
    // legacy_main. A no-op for OriginControlled responses.
    if let Some(policy) = asset_cache_policy {
        policy.apply_after_route_finalization(&mut response);
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
                    ec_finalize_response(
                        &settings,
                        &ec_state.ec_context,
                        ec_state.finalize_kv_graph.as_ref(),
                        &partner_registry,
                        ec_state.eids_cookie.as_deref(),
                        ec_state.sharedid_cookie.as_deref(),
                        &mut response,
                    );
                    if let Some(effects) = &request_filter_effects {
                        effects.apply_to_response(&mut response);
                    }
                    // Final cache guard: EC finalization and request-filter
                    // effects above may have added a per-user Set-Cookie after
                    // `apply_finalize_headers` ran, so re-apply the privacy
                    // downgrade before send, mirroring legacy_main.
                    crate::middleware::enforce_set_cookie_cache_privacy(&mut response);
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

    if let Some(effects) = &request_filter_effects {
        effects.apply_to_response(&mut response);
    }
    // Final cache guard for the no-EC-finalization fallback: request-filter
    // effects may still have added a per-user Set-Cookie after finalize headers.
    crate::middleware::enforce_set_cookie_cache_privacy(&mut response);
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
    let runtime_services = build_runtime_services(
        &req,
        std::sync::Arc::clone(&state.default_kv_store),
        std::sync::Arc::clone(&state.auction_telemetry_sink),
    );
    let http_req = compat::from_fastly_request(req);

    let route_result = futures::executor::block_on(route_request(
        &state.settings,
        &state.orchestrator,
        &state.registry,
        &partner_registry,
        &runtime_services,
        state.settings.creative_opportunity_slots(),
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
        should_finalize_ec: true,
        asset_cache_policy: AssetProxyCachePolicy::OriginControlled,
        request_filter_effects: RequestFilterEffects::default(),
    });

    let RouteResult {
        outcome,
        ec_context,
        finalize_kv_graph,
        eids_cookie,
        sharedid_cookie,
        is_real_browser,
        should_finalize_ec,
        asset_cache_policy,
        request_filter_effects,
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
            asset_cache_policy.apply_after_route_finalization(&mut response);
            if should_finalize_ec {
                ec_finalize_response(
                    &state.settings,
                    &ec_context,
                    finalize_kv_graph.as_ref(),
                    &partner_registry,
                    eids_cookie.as_deref(),
                    sharedid_cookie.as_deref(),
                    &mut response,
                );
            }
            // Apply request-filter response effects (e.g. a DataDome allow
            // Set-Cookie) before the final cache guard so any per-user cookie
            // they add is covered. EC finalization above may also have added the
            // identity Set-Cookie; the guard runs last so it observes both.
            request_filter_effects.apply_to_response(&mut response);
            let mut fastly_resp = compat::to_fastly_response(response);
            enforce_set_cookie_cache_privacy(&mut fastly_resp);
            fastly_resp.send_to_client();

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
            mut params,
        } => {
            finalize_response(&state.settings, geo_info.as_ref(), &mut response);
            asset_cache_policy.apply_after_route_finalization(&mut response);
            if should_finalize_ec {
                ec_finalize_response(
                    &state.settings,
                    &ec_context,
                    finalize_kv_graph.as_ref(),
                    &partner_registry,
                    eids_cookie.as_deref(),
                    sharedid_cookie.as_deref(),
                    &mut response,
                );
            }
            // Apply request-filter response effects (e.g. a DataDome allow
            // Set-Cookie) before the final cache guard so any per-user cookie
            // they add is covered. EC finalization above may also have added the
            // identity Set-Cookie; the guard runs last so it observes both.
            request_filter_effects.apply_to_response(&mut response);
            let mut fastly_resp = compat::to_fastly_response_skeleton(response);
            enforce_set_cookie_cache_privacy(&mut fastly_resp);
            let mut streaming_body = fastly_resp.stream_to_client();
            let mut stream_succeeded = false;
            match futures::executor::block_on(stream_publisher_body_async(
                body,
                &mut streaming_body,
                &mut params,
                &state.settings,
                &state.registry,
                &state.orchestrator,
                &runtime_services,
            )) {
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
        HandlerOutcome::AssetStreaming { mut response, body } => {
            finalize_response(&state.settings, geo_info.as_ref(), &mut response);
            asset_cache_policy.apply_after_route_finalization(&mut response);
            // A request filter (e.g. DataDome allow) can append a per-user
            // Set-Cookie via response effects even on an otherwise cacheable
            // asset, so guard against shared caching after applying them.
            request_filter_effects.apply_to_response(&mut response);
            let mut fastly_resp = compat::to_fastly_response_skeleton(response);
            enforce_set_cookie_cache_privacy(&mut fastly_resp);
            let mut streaming_body = fastly_resp.stream_to_client();
            if let Err(e) =
                futures::executor::block_on(stream_asset_body(body, &mut streaming_body))
            {
                log::error!("asset streaming failed: {e:?}");
                drop(streaming_body);
            } else if let Err(e) = streaming_body.finish() {
                log::error!("failed to finish asset streaming body: {e}");
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

// Combines the server-side ad-stack inputs (creative-opportunity `slots`) with
// the EdgeZero dual-path requirement that `device_signals` be derived from the
// `FastlyRequest` before conversion and passed in, pushing this central
// dispatch helper to eight arguments.
#[allow(clippy::too_many_arguments)]
async fn route_request(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    integration_registry: &IntegrationRegistry,
    partner_registry: &PartnerRegistry,
    runtime_services: &RuntimeServices,
    slots: &[trusted_server_core::creative_opportunities::CreativeOpportunitySlot],
    mut req: HttpRequest,
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
                    should_finalize_ec: true,
                    asset_cache_policy: AssetProxyCachePolicy::OriginControlled,
                    request_filter_effects: RequestFilterEffects::default(),
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
            should_finalize_ec: true,
            asset_cache_policy: AssetProxyCachePolicy::OriginControlled,
            request_filter_effects: RequestFilterEffects::default(),
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
                should_finalize_ec: true,
                asset_cache_policy: AssetProxyCachePolicy::OriginControlled,
                request_filter_effects: RequestFilterEffects::default(),
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
                should_finalize_ec: true,
                asset_cache_policy: AssetProxyCachePolicy::OriginControlled,
                request_filter_effects: RequestFilterEffects::default(),
            });
        }
        Ok(None) => {}
        Err(e) => return Err(e),
    }

    let request_filter_effects = match integration_registry
        .filter_request(RequestFilterRegistryInput {
            settings,
            services: runtime_services,
            req: &mut req,
            geo_info: geo_info.as_ref(),
        })
        .await
    {
        Ok(RequestFilterRegistryOutcome::Continue(effects)) => effects,
        Ok(RequestFilterRegistryOutcome::Respond { response, effects }) => {
            return Ok(RouteResult {
                outcome: HandlerOutcome::Buffered(*response),
                ec_context,
                finalize_kv_graph,
                eids_cookie,
                sharedid_cookie,
                is_real_browser,
                should_finalize_ec: true,
                asset_cache_policy: AssetProxyCachePolicy::OriginControlled,
                request_filter_effects: effects,
            });
        }
        Err(e) => {
            log::error!("Failed to run integration request filters: {:?}", e);
            return Ok(RouteResult {
                outcome: HandlerOutcome::Buffered(http_error_response(&e)),
                ec_context,
                finalize_kv_graph,
                eids_cookie,
                sharedid_cookie,
                is_real_browser,
                should_finalize_ec: true,
                asset_cache_policy: AssetProxyCachePolicy::OriginControlled,
                request_filter_effects: RequestFilterEffects::default(),
            });
        }
    };

    // Get path and method for routing
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    let registry_ref = if partner_registry.is_empty() {
        None
    } else {
        Some(partner_registry)
    };

    let mut asset_cache_policy = AssetProxyCachePolicy::OriginControlled;
    let mut should_finalize_ec = true;

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
        (Method::GET, "/_ts/set-tester") => (handle_set_tester(settings), false),
        (Method::GET, "/_ts/clear-tester") => (handle_clear_tester(settings), false),

        // Unified auction endpoint (returns creative HTML inline)
        (Method::POST, "/auction") => (
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
        ),

        // Reject CORS preflight for the side-effecting page-bids endpoint at the
        // adapter. The GET handler's legacy fallback trusts `X-TSJS-Page-Bids`
        // precisely because this endpoint never grants a preflight; letting
        // OPTIONS fall through to the publisher origin (which may return
        // permissive CORS) would defeat that, allowing a cross-site page to
        // trigger real PBS/APS auctions from a visitor's browser.
        (Method::OPTIONS, "/__ts/page-bids") => {
            let mut response = HttpResponse::new(EdgeBody::from("Forbidden"));
            *response.status_mut() = edgezero_core::http::StatusCode::FORBIDDEN;
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-store"),
            );
            (Ok(response), false)
        }

        // SPA/CSR navigation endpoint — returns slots + bids JSON for the given path
        (Method::GET, "/__ts/page-bids") => (
            handle_page_bids(
                settings,
                runtime_services,
                kv_graph.as_ref(),
                trusted_server_core::publisher::AuctionDispatch {
                    orchestrator,
                    slots,
                    registry: registry_ref,
                },
                &ec_context,
                req,
            )
            .await,
            false,
        ),

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

        // No known route matched, proxy to an asset origin or publisher origin as fallback
        (method, _) => {
            let matched_asset_route = matches!(method, Method::GET | Method::HEAD)
                .then(|| settings.asset_route_for_path(&path))
                .flatten();

            if let Some(asset_route) = matched_asset_route {
                should_finalize_ec = false;
                log::info!("No explicit route matched; proxying via configured asset route");
                let result =
                    match handle_asset_proxy_request(settings, runtime_services, req, asset_route)
                        .await
                    {
                        Ok(asset_response) => {
                            asset_cache_policy = asset_response.cache_policy();
                            let (response, stream_body) = asset_response.into_response_and_body();
                            if let Some(body) = stream_body {
                                return Ok(RouteResult {
                                    outcome: HandlerOutcome::AssetStreaming { response, body },
                                    ec_context,
                                    finalize_kv_graph,
                                    eids_cookie,
                                    sharedid_cookie,
                                    is_real_browser,
                                    should_finalize_ec,
                                    asset_cache_policy,
                                    request_filter_effects,
                                });
                            }
                            Ok(response)
                        }
                        Err(e) => {
                            asset_cache_policy = AssetProxyCachePolicy::NoStorePrivate;
                            Err(e)
                        }
                    };
                (result, false)
            } else {
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

                match handle_publisher_request(
                    settings,
                    integration_registry,
                    runtime_services,
                    kv_graph.as_ref(),
                    &mut ec_context,
                    trusted_server_core::publisher::AuctionDispatch {
                        orchestrator,
                        slots,
                        registry: registry_ref,
                    },
                    req,
                )
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
                            should_finalize_ec,
                            asset_cache_policy,
                            request_filter_effects,
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
        should_finalize_ec,
        asset_cache_policy,
        request_filter_effects,
    })
}

pub(crate) fn maybe_identity_graph(settings: &Settings) -> Option<KvIdentityGraph> {
    settings.ec.ec_store.as_ref().map(KvIdentityGraph::new)
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

pub(crate) fn resolve_publisher_response_buffered(
    publisher_response: PublisherResponse,
    method: &Method,
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
) -> Result<HttpResponse, Report<TrustedServerError>> {
    match publisher_response {
        PublisherResponse::Buffered(response) => Ok(response),
        PublisherResponse::Stream {
            mut response,
            body,
            params,
        } => {
            // HEAD and bodiless statuses (204, 304) carry no body but may
            // advertise the GET representation's length. `handle_publisher_request`
            // already stripped the origin Content-Length for processable Stream
            // responses, so rewriting it here to the buffered byte count (0)
            // would replace it with a misleading length. Skip the buffer, the
            // length rewrite, and the body replacement for those responses,
            // mirroring the asset path's `asset_response_carries_body` guard.
            if !publisher_response_carries_body(method, response.status()) {
                return Ok(response);
            }
            let mut output = BoundedWriter::new(settings.publisher.max_buffered_body_bytes);
            stream_publisher_body(body, &mut output, &params, settings, integration_registry)?;
            let bytes = output.into_inner();
            response.headers_mut().insert(
                header::CONTENT_LENGTH,
                HeaderValue::from(bytes.len() as u64),
            );
            *response.body_mut() = EdgeBody::from(bytes);
            Ok(response)
        }
        PublisherResponse::PassThrough { mut response, body } => {
            *response.body_mut() = body;
            Ok(response)
        }
    }
}

/// Returns `true` when a buffered publisher response should carry a body and a
/// recomputed `Content-Length`.
///
/// `HEAD` responses and bodiless statuses (204, 304) carry no body; rewriting
/// their `Content-Length` to the (empty) buffered length would mislead clients
/// and caches. This mirrors the asset path's `asset_response_carries_body`.
fn publisher_response_carries_body(method: &Method, status: StatusCode) -> bool {
    *method != Method::HEAD
        && status != StatusCode::NO_CONTENT
        && status != StatusCode::NOT_MODIFIED
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
    // Legacy and EdgeZero paths share one protected finalizer so the cache /
    // Set-Cookie privacy hardening cannot drift between them. `HttpResponse` and
    // the middleware's `Response` are the same `edgezero_core::http::Response`.
    apply_finalize_headers(settings, geo_info, response);
}

/// Forces cookie-bearing Fastly responses to stay private to shared caches.
///
/// [`finalize_response`] applies this same downgrade on the [`HttpResponse`],
/// but the EC identity cookie is written later by [`ec_finalize_response`] onto
/// the converted [`FastlyResponse`], so the earlier guard never sees it.
/// Re-apply it here so a first-visit navigation whose only per-user payload is
/// the EC `Set-Cookie` can never be served with `public`/surrogate cache headers
/// inherited from the origin or operator response headers — a shared cache must
/// not be able to store and replay one visitor's EC cookie to others.
///
/// Idempotent: a response already marked `private`/`no-store` keeps its stricter
/// `Cache-Control`, but the surrogate cache headers are stripped regardless so a
/// `no-store` cookie response can never retain shared Fastly cacheability.
fn enforce_set_cookie_cache_privacy(response: &mut FastlyResponse) {
    if response.get_header("set-cookie").is_none() {
        return;
    }
    // Strip surrogate cache headers on every cookie-bearing response, even when
    // keeping a stricter `no-store`/`private` directive — Surrogate-Control is
    // independent of Cache-Control and would otherwise let a shared cache store
    // and replay one visitor's Set-Cookie.
    response.remove_header(SURROGATE_CONTROL_HEADER);
    response.remove_header(FASTLY_SURROGATE_CONTROL_HEADER);
    let already_uncacheable = response
        .get_header_str("cache-control")
        .is_some_and(cache_control_is_uncacheable);
    if !already_uncacheable {
        response.set_header("cache-control", PRIVATE_CACHE_CONTROL_VALUE);
    }
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
    Ok(KvIdentityGraph::new(store_name))
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

    fn test_settings_with_empty_creative_opportunities() -> Settings {
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

            [creative_opportunities]
            gam_network_id = "12345"
            auction_timeout_ms = 500
            "#,
        )
        .expect("should parse test settings with creative opportunities")
    }

    fn test_settings_with_configured_creative_opportunities() -> Settings {
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

            [creative_opportunities]
            gam_network_id = "12345"
            auction_timeout_ms = 500

            [[creative_opportunities.slot]]
            id = "atf"
            page_patterns = ["/article/*"]
            formats = [{ width = 300, height = 250 }]
            "#,
        )
        .expect("should parse test settings with configured creative opportunities")
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

    #[test]
    fn edgezero_accepts_settings_without_creative_opportunities() {
        let settings = test_settings();

        assert!(
            edgezero_can_handle_settings(&settings),
            "should allow EdgeZero when server-side ad templates are not configured"
        );
    }

    #[test]
    fn edgezero_accepts_settings_with_empty_creative_opportunities() {
        let settings = test_settings_with_empty_creative_opportunities();

        assert!(
            edgezero_can_handle_settings(&settings),
            "should allow EdgeZero when server-side ad templates are configured but no slots are enabled"
        );
    }

    #[test]
    fn edgezero_rejects_settings_with_configured_creative_opportunity_slots() {
        let settings = test_settings_with_configured_creative_opportunities();

        assert!(
            !edgezero_can_handle_settings(&settings),
            "should route through legacy path while EdgeZero lacks server-side ad-template support"
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
    fn publisher_response_carries_body_preserves_bodiless_content_length() {
        // A processable GET 200 publisher response buffers a body and recomputes
        // Content-Length.
        assert!(
            super::publisher_response_carries_body(&Method::GET, StatusCode::OK),
            "a GET 200 publisher response should carry a buffered body"
        );
        // HEAD responses carry no body; recomputing Content-Length to 0 would
        // mislead clients/caches about the GET representation length.
        assert!(
            !super::publisher_response_carries_body(&Method::HEAD, StatusCode::OK),
            "HEAD publisher responses must not get a recomputed Content-Length"
        );
        // Bodiless statuses keep their metadata regardless of method.
        assert!(
            !super::publisher_response_carries_body(&Method::GET, StatusCode::NO_CONTENT),
            "204 responses must not get a recomputed Content-Length"
        );
        assert!(
            !super::publisher_response_carries_body(&Method::GET, StatusCode::NOT_MODIFIED),
            "304 responses must not get a recomputed Content-Length"
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
