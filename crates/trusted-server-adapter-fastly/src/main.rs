use std::sync::Arc;

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

use trusted_server_core::ec::device::DeviceSignals;
use trusted_server_core::ec::finalize::ec_finalize_response;
use trusted_server_core::ec::kv::KvIdentityGraph;
use trusted_server_core::ec::pull_sync::{
    build_pull_sync_context, dispatch_pull_sync, PullSyncContext,
};
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::integrations::RequestFilterEffects;
use trusted_server_core::platform::PlatformGeo as _;
use trusted_server_core::platform::RuntimeServices;
use trusted_server_core::proxy::{stream_asset_body, AssetProxyCachePolicy};
use trusted_server_core::publisher::stream_publisher_body;
use trusted_server_core::settings::Settings;

mod app;
mod backend;
mod compat;
mod ec_kv;
mod logging;
mod management_api;
mod middleware;
mod platform;
mod rate_limiter;

use crate::app::{
    load_settings_from_config_store, EcFinalizeState, PublisherStreamState, TrustedServerApp,
};
use crate::ec_kv::FastlyEcKvStore;
use crate::middleware::{apply_finalize_headers, resolve_geo_for_response, HEADER_X_TS_FINALIZED};
use crate::platform::{client_info_from_request, FastlyPlatformGeo};
use crate::rate_limiter::{FastlyRateLimiter, RATE_COUNTER_NAME};

const TRUSTED_SERVER_CONFIG_STORE: &str = "trusted_server_config";

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
    let settings_snapshot = app_state.as_ref().map(|state| Arc::clone(&state.settings));

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

    // Capture client IP before the request is consumed by dispatch.
    let client_ip = req.get_client_ip_addr();

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

    if !take_finalize_sentinel(&mut response) {
        if let Some(settings) = settings_snapshot.as_deref() {
            apply_entry_point_finalize_headers(settings, &mut response, client_ip);
        } else {
            match load_settings_from_config_store() {
                Ok(settings) => {
                    apply_entry_point_finalize_headers(&settings, &mut response, client_ip);
                }
                Err(e) => {
                    log::warn!("entry-point finalize skipped: failed to reload settings: {e:?}");
                }
            }
        }
    }

    if let Some(policy) = asset_cache_policy {
        policy.apply_after_route_finalization(&mut response);
    }

    if let Some(ec_state) = ec_state {
        if let Some(settings) = settings_snapshot.as_deref() {
            match apply_edgezero_ec_finalize(settings, &ec_state, &mut response) {
                Ok(partner_registry) => {
                    send_edgezero_response(response, request_filter_effects.as_ref());
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
                    match apply_edgezero_ec_finalize(&settings, &ec_state, &mut response) {
                        Ok(partner_registry) => {
                            send_edgezero_response(response, request_filter_effects.as_ref());
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

    send_edgezero_response(response, request_filter_effects.as_ref());
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
) {
    let geo_info = resolve_geo_for_response(response, client_ip, |client_ip| {
        FastlyPlatformGeo.lookup(client_ip).unwrap_or_else(|e| {
            log::warn!("entry-point geo lookup failed: {e}");
            None
        })
    });
    apply_finalize_headers(settings, geo_info.as_ref(), response);
}

fn apply_edgezero_ec_finalize(
    settings: &Settings,
    ec_state: &EcFinalizeState,
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
    if ec_state.is_real_browser {
        if let Some(context) = build_pull_sync_context(&ec_state.ec_context) {
            run_pull_sync_after_send(settings, partner_registry, &context, &ec_state.services);
        }
    }
}

/// Sends a finalized `EdgeZero` response to the client.
///
/// Publisher and asset streams commit headers first, then pipe the origin body
/// chunk by chunk so large responses do not materialize in the Wasm heap.
/// Other responses are sent in one shot.
fn send_edgezero_response(
    mut response: HttpResponse,
    request_filter_effects: Option<&RequestFilterEffects>,
) {
    if let Some(effects) = request_filter_effects {
        effects.apply_to_response(&mut response);
    }

    let publisher_stream = response
        .extensions_mut()
        .remove::<Arc<PublisherStreamState>>();
    let (parts, body) = response.into_parts();

    if let Some(stream_state) = publisher_stream {
        let skeleton =
            compat::to_fastly_response_skeleton(HttpResponse::from_parts(parts, EdgeBody::empty()));
        let mut streaming_body = skeleton.stream_to_client();
        match stream_publisher_body(
            body,
            &mut streaming_body,
            &stream_state.params,
            &stream_state.settings,
            &stream_state.registry,
        ) {
            Ok(()) => {
                if let Err(e) = streaming_body.finish() {
                    log::error!("failed to finish EdgeZero publisher streaming body: {e}");
                }
            }
            Err(e) => {
                log::error!("EdgeZero publisher streaming failed: {e:?}");
                drop(streaming_body);
            }
        }
        return;
    }

    match body {
        EdgeBody::Stream(_) => {
            let skeleton = compat::to_fastly_response_skeleton(HttpResponse::from_parts(
                parts,
                EdgeBody::empty(),
            ));
            let mut streaming_body = skeleton.stream_to_client();
            match futures::executor::block_on(stream_asset_body(body, &mut streaming_body)) {
                Ok(()) => {
                    if let Err(e) = streaming_body.finish() {
                        log::error!("failed to finish EdgeZero asset streaming body: {e}");
                    }
                }
                Err(e) => {
                    log::error!("EdgeZero asset streaming failed: {e:?}");
                    drop(streaming_body);
                }
            }
        }
        once => {
            compat::to_fastly_response(HttpResponse::from_parts(parts, once)).send_to_client();
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
    use edgezero_core::body::Body as EdgeBody;
    use edgezero_core::http::response_builder;
    use edgezero_core::http::HeaderValue;
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
