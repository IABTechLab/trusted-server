use edgezero_core::body::Body as EdgeBody;
use edgezero_core::http::{
    header, HeaderName, HeaderValue, Method, Request as HttpRequest, Response as HttpResponse,
};
use error_stack::Report;
use fastly::http::Method as FastlyMethod;
use fastly::{Request as FastlyRequest, Response as FastlyResponse};

use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{build_orchestrator, AuctionOrchestrator};
use trusted_server_core::auth::enforce_basic_auth;
use trusted_server_core::compat;
use trusted_server_core::constants::{
    ENV_FASTLY_IS_STAGING, ENV_FASTLY_SERVICE_VERSION, HEADER_X_GEO_INFO_AVAILABLE,
    HEADER_X_TS_ENV, HEADER_X_TS_VERSION,
};
use trusted_server_core::error::{IntoHttpResponse, TrustedServerError};
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::integrations::IntegrationRegistry;
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

mod error;
mod logging;
mod management_api;
mod platform;
#[cfg(test)]
mod route_tests;

use crate::error::to_error_response;
use crate::logging::init_logger;
use crate::platform::{build_runtime_services, open_kv_store, UnavailableKvStore};

/// Result of routing a request, distinguishing buffered from streaming publisher responses.
///
/// The streaming arm keeps the publisher body out of WASM heap until it is written directly
/// to the client via [`fastly::Response::stream_to_client`].  All other routes are buffered.
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

/// Entry point for the Fastly Compute program.
///
/// Uses an undecorated `main()` with `FastlyRequest::from_client()` instead of
/// `#[fastly::main]` so we can call `send_to_client()` explicitly when needed.
fn main() {
    init_logger();

    let mut req = FastlyRequest::from_client();

    // Keep the health probe independent from settings loading and routing so
    // readiness checks still get a cheap liveness response during startup.
    if req.get_method() == FastlyMethod::GET && req.get_path() == "/health" {
        FastlyResponse::from_status(200)
            .with_body_text_plain("ok")
            .send_to_client();
        return;
    }

    let settings = match get_settings() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to load settings: {:?}", e);
            to_error_response(&e).send_to_client();
            return;
        }
    };
    log::debug!("Settings {settings:?}");

    // Short-circuit the ja4 debug probe before finalize_response so that
    // Cache-Control: no-store, private cannot be replaced by operator [response_headers].
    if req.get_method() == FastlyMethod::GET && req.get_path() == "/_ts/debug/ja4" {
        if settings.debug.ja4_endpoint_enabled {
            build_ja4_debug_response(&req).send_to_client();
        } else {
            FastlyResponse::from_status(fastly::http::StatusCode::NOT_FOUND).send_to_client();
        }
        return;
    }

    // Build the auction orchestrator once at startup
    let orchestrator = match build_orchestrator(&settings) {
        Ok(o) => o,
        Err(e) => {
            log::error!("Failed to build auction orchestrator: {:?}", e);
            to_error_response(&e).send_to_client();
            return;
        }
    };

    let integration_registry = match IntegrationRegistry::new(&settings) {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to create integration registry: {:?}", e);
            to_error_response(&e).send_to_client();
            return;
        }
    };

    let kv_store = std::sync::Arc::new(UnavailableKvStore)
        as std::sync::Arc<dyn trusted_server_core::platform::PlatformKvStore>;
    // Strip client-spoofable forwarded headers at the edge before building
    // any request-derived context or converting to the core HTTP types.
    compat::sanitize_fastly_forwarded_headers(&mut req);

    let runtime_services = build_runtime_services(&req, kv_store);
    let http_req = compat::from_fastly_request(req);

    let outcome = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &runtime_services,
        http_req,
    ))
    .unwrap_or_else(|e| HandlerOutcome::Buffered(http_error_response(&e)));

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
            finalize_response(&settings, geo_info.as_ref(), &mut response);
            compat::to_fastly_response(response).send_to_client();
        }
        HandlerOutcome::Streaming {
            mut response,
            body,
            params,
        } => {
            finalize_response(&settings, geo_info.as_ref(), &mut response);
            let fastly_resp = compat::to_fastly_response_skeleton(response);
            let mut streaming_body = fastly_resp.stream_to_client();
            match stream_publisher_body(
                body,
                &mut streaming_body,
                &params,
                &settings,
                &integration_registry,
            ) {
                Ok(()) => {
                    if let Err(e) = streaming_body.finish() {
                        log::error!("failed to finish streaming body: {e}");
                    }
                }
                Err(e) => {
                    log::error!("streaming processing failed: {e:?}");
                    // Headers already committed. Drop the body so the client sees a
                    // truncated response (EOF mid-stream) — standard proxy behavior.
                    drop(streaming_body);
                }
            }
        }
    }
}

const FALLBACK_UNAVAILABLE: &str = "unavailable";
const FALLBACK_NOT_SENT: &str = "not sent";
const FALLBACK_NONE: &str = "none";

// TODO: remove after JA4 evaluation completes — see #645
fn build_ja4_debug_response(req: &FastlyRequest) -> FastlyResponse {
    let ja4 = req.get_tls_ja4().unwrap_or(FALLBACK_UNAVAILABLE);
    let h2 = req
        .get_client_h2_fingerprint()
        .unwrap_or(FALLBACK_UNAVAILABLE);
    let cipher = req
        .get_tls_cipher_openssl_name()
        .unwrap_or(FALLBACK_UNAVAILABLE);
    let tls_version = req.get_tls_protocol().unwrap_or(FALLBACK_UNAVAILABLE);
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
        Ok(Some(response)) => return Ok(HandlerOutcome::AuthChallenge(response)),
        Ok(None) => {}
        Err(e) => return Err(e),
    }

    // Get path and method for routing
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    // Match known routes and handle them
    match (method, path.as_str()) {
        // Serve the tsjs library
        (Method::GET, path) if path.starts_with("/static/tsjs=") => {
            handle_tsjs_dynamic(&req, integration_registry).map(HandlerOutcome::Buffered)
        }

        // Discovery endpoint for trusted-server capabilities and JWKS
        (Method::GET, "/.well-known/trusted-server.json") => {
            handle_trusted_server_discovery(settings, runtime_services, req)
                .map(HandlerOutcome::Buffered)
        }

        // Signature verification endpoint
        (Method::POST, "/verify-signature") => {
            handle_verify_signature(settings, runtime_services, req).map(HandlerOutcome::Buffered)
        }

        // Key rotation admin endpoints
        // Keep in sync with Settings::ADMIN_ENDPOINTS in crates/trusted-server-core/src/settings.rs
        (Method::POST, "/admin/keys/rotate") => {
            handle_rotate_key(settings, runtime_services, req).map(HandlerOutcome::Buffered)
        }
        (Method::POST, "/admin/keys/deactivate") => {
            handle_deactivate_key(settings, runtime_services, req).map(HandlerOutcome::Buffered)
        }

        // Unified auction endpoint (returns creative HTML inline)
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

        // tsjs endpoints
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
        (m, path) if integration_registry.has_route(&m, path) => {
            // TODO(PR13): migrate integration trait to http types here
            integration_registry
                .handle_proxy(
                    &m,
                    path,
                    settings,
                    runtime_services,
                    compat::to_fastly_request(req),
                )
                .await
                .unwrap_or_else(|| {
                    Err(Report::new(TrustedServerError::BadRequest {
                        message: format!("Unknown integration route: {path}"),
                    }))
                })
                .map(compat::from_fastly_response)
                .map(HandlerOutcome::Buffered)
        }

        // No known route matched, proxy to publisher origin as fallback
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

fn runtime_services_for_consent_route(
    settings: &Settings,
    runtime_services: &RuntimeServices,
) -> Result<RuntimeServices, Report<TrustedServerError>> {
    let Some(store_name) = settings.consent.consent_store.as_deref() else {
        return Ok(runtime_services.clone());
    };

    open_kv_store(store_name)
        .map(|store| runtime_services.clone().with_kv_store(store))
        .map_err(|e| {
            Report::new(TrustedServerError::KvStore {
                store_name: store_name.to_string(),
                message: e.to_string(),
            })
        })
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
    if let Some(geo) = geo_info {
        geo.set_response_headers(response);
    } else {
        response.headers_mut().insert(
            HEADER_X_GEO_INFO_AVAILABLE,
            HeaderValue::from_static("false"),
        );
    }

    if let Ok(v) = ::std::env::var(ENV_FASTLY_SERVICE_VERSION) {
        if let Ok(value) = HeaderValue::from_str(&v) {
            response.headers_mut().insert(HEADER_X_TS_VERSION, value);
        } else {
            log::warn!("Skipping invalid FASTLY_SERVICE_VERSION response header value");
        }
    }
    if ::std::env::var(ENV_FASTLY_IS_STAGING).as_deref() == Ok("1") {
        response
            .headers_mut()
            .insert(HEADER_X_TS_ENV, HeaderValue::from_static("staging"));
    }

    for (key, value) in &settings.response_headers {
        let header_name = HeaderName::from_bytes(key.as_bytes())
            .expect("settings.response_headers validated at load time");
        let header_value =
            HeaderValue::from_str(value).expect("settings.response_headers validated at load time");
        response.headers_mut().insert(header_name, header_value);
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

#[cfg(test)]
mod tests {
    use super::*;
    use fastly::mime;

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
