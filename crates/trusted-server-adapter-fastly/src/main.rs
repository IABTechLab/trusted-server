use std::net::IpAddr;
use std::sync::Arc;

use edgezero_adapter_fastly::FastlyConfigStore;
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
use trusted_server_core::error::{IntoHttpResponse, TrustedServerError};
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::integrations::IntegrationRegistry;
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
mod error;
mod logging;
mod management_api;
mod middleware;
mod platform;
#[cfg(test)]
mod route_tests;

use crate::app::{build_state, runtime_services_for_consent_route, TrustedServerApp};
use crate::error::to_error_response;
use crate::middleware::{apply_finalize_headers, HEADER_X_TS_FINALIZED};
use crate::platform::{build_runtime_services, FastlyPlatformGeo};

const TRUSTED_SERVER_CONFIG_STORE: &str = "trusted_server_config";
const EDGEZERO_ENABLED_KEY: &str = "edgezero_enabled";
const EDGEZERO_ROLLOUT_PCT_KEY: &str = "edgezero_rollout_pct";

/// Result of routing a request, distinguishing buffered from streaming publisher responses.
///
/// The streaming arm keeps the publisher body out of WASM heap until it is written directly
/// to the client via [`fastly::Response::stream_to_client`]. All other legacy routes are buffered.
enum HandlerOutcome {
    Buffered(HttpResponse),
    Streaming {
        response: HttpResponse,
        body: EdgeBody,
        params: OwnedProcessResponseParams,
    },
}

impl HandlerOutcome {
    fn status(&self) -> edgezero_core::http::StatusCode {
        match self {
            HandlerOutcome::Buffered(resp) => resp.status(),
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
fn canary_routes_to_edgezero(bucket: u8, rollout_pct: u8) -> bool {
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
            // Fires per-request when key is absent and edgezero_enabled=true.
            // At production scale this creates one warn per request until the key is set.
            // Resolution: set edgezero_rollout_pct = "0" before setting edgezero_enabled = "true".
            log::warn!(
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

fn health_response(req: &FastlyRequest) -> Option<FastlyResponse> {
    if req.get_method() == FastlyMethod::GET && req.get_path() == "/health" {
        return Some(FastlyResponse::from_status(200).with_body_text_plain("ok"));
    }

    None
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
    let routing_key = match req.get_client_ip_addr() {
        Some(ip) => ip.to_string(),
        None => {
            log::debug!(
                "no client IP available, using empty routing key (deterministic bucket 61)"
            );
            String::new()
        }
    };
    let bucket = fnv1a_bucket(&routing_key);

    if canary_routes_to_edgezero(bucket, rollout_pct) {
        log::debug!(
            "routing request through EdgeZero path (bucket={bucket}, rollout_pct={rollout_pct})"
        );
        edgezero_main(req, edgezero_config_store);
    } else {
        log::debug!(
            "routing request through legacy path (bucket={bucket}, rollout_pct={rollout_pct})"
        );
        legacy_main(req);
    }
}

/// Handles a request through the `EdgeZero` router path.
fn edgezero_main(mut req: FastlyRequest, config_store: ConfigStoreHandle) {
    let app = TrustedServerApp::build_app();

    // Strip client-spoofable forwarded headers before handing off to the
    // EdgeZero dispatcher, mirroring the sanitization done in legacy_main.
    compat::sanitize_fastly_forwarded_headers(&mut req);

    // Capture client IP before the request is consumed by dispatch.
    let client_ip = req.get_client_ip_addr();

    // `run_app_with_config` and `run_app_with_logging` call `init_logger`
    // internally. A second `set_logger` call panics because our custom fern
    // logger is already initialised above. `dispatch_with_config_handle` skips logger
    // initialisation and injects the config store directly.
    let mut response =
        match edgezero_adapter_fastly::dispatch_with_config_handle(&app, req, config_store) {
            Ok(response) => compat::from_fastly_response(response),
            Err(e) => {
                log::error!("EdgeZero dispatch failed: {e}");
                FastlyResponse::from_status(fastly::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_body_text_plain("Internal Server Error")
                    .send_to_client();
                return;
            }
        };

    if !response_was_finalized_by_middleware(&mut response) {
        // Apply finalize headers at the entry point so that router-level
        // 405/404 responses for unregistered HTTP methods (e.g. TRACE, WebDAV
        // verbs) carry TS/geo headers. Middleware-finalized responses are
        // skipped here to avoid a second settings read and geo lookup on the
        // normal registered-route path.
        match get_settings() {
            Ok(settings) => {
                apply_entry_point_finalize(&settings, client_ip, &mut response, |client_ip| {
                    FastlyPlatformGeo.lookup(client_ip).unwrap_or_else(|e| {
                        log::warn!("entry-point geo lookup failed: {e}");
                        None
                    })
                })
            }
            Err(e) => {
                log::warn!("entry-point finalize skipped: failed to reload settings: {e:?}");
            }
        }
    }

    compat::to_fastly_response(response).send_to_client();
}

fn response_was_finalized_by_middleware(response: &mut HttpResponse) -> bool {
    response
        .headers_mut()
        .remove(HEADER_X_TS_FINALIZED)
        .is_some()
}

fn apply_entry_point_finalize<F>(
    settings: &Settings,
    client_ip: Option<IpAddr>,
    response: &mut HttpResponse,
    lookup_geo: F,
) where
    F: FnOnce(Option<IpAddr>) -> Option<GeoInfo>,
{
    let geo_info = if response.status() == edgezero_core::http::StatusCode::UNAUTHORIZED {
        None
    } else {
        lookup_geo(client_ip)
    };
    apply_finalize_headers(settings, geo_info.as_ref(), response);
}

/// Handles a request using the original Fastly-native entry point.
///
/// Preserves identical semantics to the pre-PR14 `main()`. Called when
/// the `edgezero_enabled` config flag is absent or `false`.
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

    // Strip client-spoofable forwarded headers at the edge before building
    // any request-derived context or converting to the core HTTP types.
    compat::sanitize_fastly_forwarded_headers(&mut req);

    let runtime_services = build_runtime_services(&req, std::sync::Arc::clone(&state.kv_store));
    let http_req = compat::from_fastly_request(req);

    let outcome = futures::executor::block_on(route_request(
        &state.settings,
        &state.orchestrator,
        &state.registry,
        &runtime_services,
        http_req,
    ))
    .unwrap_or_else(|e| HandlerOutcome::Buffered(http_error_response(&e)));

    // Skip geo lookup for 401s: avoids exposing geo headers to unauthenticated callers.
    let geo_info = if outcome.status() == edgezero_core::http::StatusCode::UNAUTHORIZED {
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
        HandlerOutcome::Buffered(mut response) => {
            finalize_response(&state.settings, geo_info.as_ref(), &mut response);
            compat::to_fastly_response(response).send_to_client();
        }
        HandlerOutcome::Streaming {
            mut response,
            body,
            params,
        } => {
            finalize_response(&state.settings, geo_info.as_ref(), &mut response);
            let fastly_resp = compat::to_fastly_response_skeleton(response);
            let mut streaming_body = fastly_resp.stream_to_client();
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
                    }
                }
                Err(e) => {
                    log::error!("streaming processing failed: {e:?}");
                    if let Err(finish_err) = streaming_body.finish() {
                        log::error!("failed to finish streaming body after error: {finish_err}");
                    }
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
    runtime_services: &RuntimeServices,
    req: HttpRequest,
) -> Result<HandlerOutcome, Report<TrustedServerError>> {
    // `get_settings()` should already have rejected invalid handler regexes.
    // Keep this fallback so manually-constructed or otherwise unprepared
    // settings still become an error response instead of panicking.
    match enforce_basic_auth(settings, &req) {
        Ok(Some(response)) => return Ok(HandlerOutcome::Buffered(response)),
        Ok(None) => {}
        Err(e) => return Err(e),
    }

    // Get path and method for routing.
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    // Match known routes and handle them.
    match (method, path.as_str()) {
        // Serve the tsjs library.
        (Method::GET, path) if path.starts_with("/static/tsjs=") => {
            handle_tsjs_dynamic(&req, integration_registry).map(HandlerOutcome::Buffered)
        }

        // Discovery endpoint for trusted-server capabilities and JWKS.
        (Method::GET, "/.well-known/trusted-server.json") => {
            handle_trusted_server_discovery(settings, runtime_services, req)
                .map(HandlerOutcome::Buffered)
        }

        // Signature verification endpoint.
        (Method::POST, "/verify-signature") => {
            handle_verify_signature(settings, runtime_services, req).map(HandlerOutcome::Buffered)
        }

        // Key rotation admin endpoints.
        // Keep in sync with Settings::ADMIN_ENDPOINTS in crates/trusted-server-core/src/settings.rs
        (Method::POST, "/admin/keys/rotate") => {
            handle_rotate_key(settings, runtime_services, req).map(HandlerOutcome::Buffered)
        }
        (Method::POST, "/admin/keys/deactivate") => {
            handle_deactivate_key(settings, runtime_services, req).map(HandlerOutcome::Buffered)
        }

        // Unified auction endpoint.
        (Method::POST, "/auction") => {
            match runtime_services_for_consent_route(settings, runtime_services) {
                Ok(auction_services) => {
                    handle_auction(settings, orchestrator, &auction_services, req)
                        .await
                        .map(HandlerOutcome::Buffered)
                }
                Err(e) => Err(e),
            }
        }

        // tsjs endpoints.
        (Method::GET, "/first-party/proxy") => {
            handle_first_party_proxy(settings, runtime_services, req)
                .await
                .map(HandlerOutcome::Buffered)
        }
        (Method::GET, "/first-party/click") => {
            handle_first_party_click(settings, runtime_services, req)
                .await
                .map(HandlerOutcome::Buffered)
        }
        (Method::GET, "/first-party/sign") | (Method::POST, "/first-party/sign") => {
            handle_first_party_proxy_sign(settings, runtime_services, req)
                .await
                .map(HandlerOutcome::Buffered)
        }
        (Method::POST, "/first-party/proxy-rebuild") => {
            handle_first_party_proxy_rebuild(settings, runtime_services, req)
                .await
                .map(HandlerOutcome::Buffered)
        }
        (m, path) if integration_registry.has_route(&m, path) => integration_registry
            .handle_proxy(&m, path, settings, runtime_services, req)
            .await
            .unwrap_or_else(|| {
                Err(Report::new(TrustedServerError::BadRequest {
                    message: format!("Unknown integration route: {path}"),
                }))
            })
            .map(HandlerOutcome::Buffered),

        // No known route matched, proxy to publisher origin as fallback.
        _ => {
            log::info!(
                "No known route matched for path: {}, proxying to publisher origin",
                path
            );

            match runtime_services_for_consent_route(settings, runtime_services) {
                Ok(publisher_services) => handle_publisher_request(
                    settings,
                    integration_registry,
                    &publisher_services,
                    req,
                )
                .await
                .and_then(resolve_publisher_response),
                Err(e) => Err(e),
            }
        }
    }
}

fn resolve_publisher_response(
    publisher_response: PublisherResponse,
) -> Result<HandlerOutcome, Report<TrustedServerError>> {
    match publisher_response {
        PublisherResponse::Buffered(response) => Ok(HandlerOutcome::Buffered(response)),
        PublisherResponse::Stream {
            response,
            body,
            params,
        } => Ok(HandlerOutcome::Streaming {
            response,
            body,
            params,
        }),
        PublisherResponse::PassThrough { mut response, body } => {
            *response.body_mut() = body;
            Ok(HandlerOutcome::Buffered(response))
        }
    }
}

pub(crate) fn resolve_publisher_response_buffered(
    publisher_response: PublisherResponse,
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
            let mut output = Vec::new();
            stream_publisher_body(body, &mut output, &params, settings, integration_registry)?;
            response.headers_mut().insert(
                header::CONTENT_LENGTH,
                HeaderValue::from(output.len() as u64),
            );
            *response.body_mut() = EdgeBody::from(output);
            Ok(response)
        }
        PublisherResponse::PassThrough { mut response, body } => {
            *response.body_mut() = body;
            Ok(response)
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use edgezero_core::http::response_builder;
    use fastly::mime;

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
    // canary_routes_to_edgezero
    // ---------------------------------------------------------------------------

    #[test]
    fn rollout_zero_routes_all_to_legacy() {
        for bucket in 0u8..100 {
            assert!(
                !canary_routes_to_edgezero(bucket, 0),
                "pct=0 should route all to legacy, bucket={bucket}"
            );
        }
    }

    #[test]
    fn rollout_hundred_routes_all_to_edgezero() {
        for bucket in 0u8..100 {
            assert!(
                canary_routes_to_edgezero(bucket, 100),
                "pct=100 should route all to EdgeZero, bucket={bucket}"
            );
        }
    }

    #[test]
    fn rollout_fifty_routes_exactly_half_of_bucket_space() {
        let edgezero_count = (0u8..100)
            .filter(|&b| canary_routes_to_edgezero(b, 50))
            .count();
        assert_eq!(
            edgezero_count, 50,
            "pct=50 should route exactly 50 out of 100 buckets to EdgeZero"
        );
    }

    #[test]
    fn rollout_one_routes_exactly_one_bucket() {
        let edgezero_count = (0u8..100)
            .filter(|&b| canary_routes_to_edgezero(b, 1))
            .count();
        assert_eq!(
            edgezero_count, 1,
            "pct=1 should route exactly 1 out of 100 buckets to EdgeZero"
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
    fn response_was_finalized_by_middleware_strips_sentinel() {
        let mut response = HttpResponse::new(EdgeBody::empty());
        response
            .headers_mut()
            .insert("x-ts-finalized", HeaderValue::from_static("1"));

        assert!(
            response_was_finalized_by_middleware(&mut response),
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
        let settings = get_settings().expect("should load settings");
        let mut response = response_builder()
            .status(edgezero_core::http::StatusCode::UNAUTHORIZED)
            .body(EdgeBody::empty())
            .expect("should build response");

        apply_entry_point_finalize(&settings, None, &mut response, |_| {
            panic!("should skip entry-point geo lookup for 401 responses");
        });

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
