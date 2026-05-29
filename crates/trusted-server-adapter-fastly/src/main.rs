use error_stack::Report;
use fastly::http::{header, Method};
use fastly::{Request, Response};

use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{build_orchestrator, AuctionOrchestrator};
use trusted_server_core::auth::enforce_basic_auth;
use trusted_server_core::compat;
use trusted_server_core::constants::{
    ENV_FASTLY_IS_STAGING, ENV_FASTLY_SERVICE_VERSION, HEADER_X_GEO_INFO_AVAILABLE,
    HEADER_X_TS_ENV, HEADER_X_TS_VERSION,
};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::platform::RuntimeServices;
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    handle_page_bids, handle_publisher_request, handle_tsjs_dynamic, stream_publisher_body_async,
    PublisherResponse,
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

/// Entry point for the Fastly Compute program.
///
/// Uses an undecorated `main()` with `Request::from_client()` instead of
/// `#[fastly::main]` so we can call `stream_to_client()` or `send_to_client()`
/// explicitly. `#[fastly::main]` is syntactic sugar that auto-calls
/// `send_to_client()` on the returned `Response`, which is incompatible with
/// streaming.
fn main() {
    init_logger();

    let req = Request::from_client();

    // Keep the health probe independent from settings loading and routing so
    // readiness checks still get a cheap liveness response during startup.
    if req.get_method() == Method::GET && req.get_path() == "/health" {
        Response::from_status(200)
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
    if req.get_method() == Method::GET && req.get_path() == "/_ts/debug/ja4" {
        if settings.debug.ja4_endpoint_enabled {
            build_ja4_debug_response(&req).send_to_client();
        } else {
            Response::from_status(fastly::http::StatusCode::NOT_FOUND).send_to_client();
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

    // Start with an unavailable KV slot. Consent-dependent routes lazily
    // replace it with the configured store at dispatch time so unrelated
    // routes stay available when consent persistence is misconfigured.
    let kv_store = std::sync::Arc::new(UnavailableKvStore)
        as std::sync::Arc<dyn trusted_server_core::platform::PlatformKvStore>;
    let runtime_services = build_runtime_services(&req, kv_store);

    // route_request may send the response directly (streaming path) or
    // return it for us to send (buffered path).
    if let Some(response) = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &runtime_services,
        settings.creative_opportunity_slots(),
        req,
    )) {
        response.send_to_client();
    }
}

const FALLBACK_UNAVAILABLE: &str = "unavailable";
const FALLBACK_NOT_SENT: &str = "not sent";
const FALLBACK_NONE: &str = "none";

// TODO: remove after JA4 evaluation completes — see #645
fn build_ja4_debug_response(req: &Request) -> Response {
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

    Response::from_status(fastly::http::StatusCode::OK)
        .with_header(header::CACHE_CONTROL, "no-store, private")
        .with_header(
            header::VARY,
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
    slots: &[trusted_server_core::creative_opportunities::CreativeOpportunitySlot],
    mut req: Request,
) -> Option<Response> {
    // Strip client-spoofable forwarded headers at the edge.
    // On Fastly this service IS the first proxy — these headers from
    // clients are untrusted and can hijack URL rewriting (see #409).
    compat::sanitize_fastly_forwarded_headers(&mut req);

    // Look up geo info via the platform abstraction using the client IP
    // already captured in RuntimeServices at the entry point.
    let geo_info = runtime_services
        .geo()
        .lookup(runtime_services.client_info().client_ip)
        .unwrap_or_else(|e| {
            log::warn!("geo lookup failed: {e}");
            None
        });

    // `get_settings()` should already have rejected invalid handler regexes.
    // Keep this fallback so manually-constructed or otherwise unprepared
    // settings still become an error response instead of panicking.
    let auth_req = compat::from_fastly_headers_ref(&req);
    match enforce_basic_auth(settings, &auth_req) {
        Ok(Some(response)) => {
            let mut response = compat::to_fastly_response(response);
            finalize_response(settings, geo_info.as_ref(), &mut response);
            return Some(response);
        }
        Ok(None) => {}
        Err(e) => {
            log::error!("Failed to evaluate basic auth: {:?}", e);
            let mut response = to_error_response(&e);
            finalize_response(settings, geo_info.as_ref(), &mut response);
            return Some(response);
        }
    }

    // Get path and method for routing
    let path = req.get_path().to_string();
    let method = req.get_method().clone();

    // Match known routes and handle them
    let result = match (method, path.as_str()) {
        // Serve the tsjs library
        (Method::GET, path) if path.starts_with("/static/tsjs=") => {
            handle_tsjs_dynamic(&req, integration_registry)
        }

        // Discovery endpoint for trusted-server capabilities and JWKS
        (Method::GET, "/.well-known/trusted-server.json") => {
            handle_trusted_server_discovery(settings, runtime_services, req)
        }

        // Signature verification endpoint
        (Method::POST, "/verify-signature") => {
            handle_verify_signature(settings, runtime_services, req)
        }

        // Key rotation admin endpoints
        // Keep in sync with Settings::ADMIN_ENDPOINTS in crates/trusted-server-core/src/settings.rs
        (Method::POST, "/admin/keys/rotate") => handle_rotate_key(settings, runtime_services, req),
        (Method::POST, "/admin/keys/deactivate") => {
            handle_deactivate_key(settings, runtime_services, req)
        }

        // Unified auction endpoint (returns creative HTML inline)
        (Method::POST, "/auction") => {
            match runtime_services_for_consent_route(settings, runtime_services) {
                Ok(auction_services) => {
                    handle_auction(settings, orchestrator, &auction_services, req).await
                }
                Err(e) => Err(e),
            }
        }

        // SPA/CSR navigation endpoint — returns slots + bids JSON for the given path
        (Method::GET, "/__ts/page-bids") => {
            match runtime_services_for_consent_route(settings, runtime_services) {
                Ok(publisher_services) => {
                    handle_page_bids(settings, orchestrator, &publisher_services, slots, req).await
                }
                Err(e) => Err(e),
            }
        }

        // tsjs endpoints
        (Method::GET, "/first-party/proxy") => {
            handle_first_party_proxy(settings, runtime_services, req).await
        }
        (Method::GET, "/first-party/click") => {
            handle_first_party_click(settings, runtime_services, req).await
        }
        (Method::GET, "/first-party/sign") | (Method::POST, "/first-party/sign") => {
            handle_first_party_proxy_sign(settings, runtime_services, req).await
        }
        (Method::POST, "/first-party/proxy-rebuild") => {
            handle_first_party_proxy_rebuild(settings, runtime_services, req).await
        }
        (m, path) if integration_registry.has_route(&m, path) => integration_registry
            .handle_proxy(&m, path, settings, runtime_services, req)
            .await
            .unwrap_or_else(|| {
                Err(Report::new(TrustedServerError::BadRequest {
                    message: format!("Unknown integration route: {path}"),
                }))
            }),

        // No known route matched, proxy to publisher origin as fallback
        _ => {
            log::info!(
                "No known route matched for path: {}, proxying to publisher origin",
                path
            );

            match runtime_services_for_consent_route(settings, runtime_services) {
                Ok(publisher_services) => {
                    match handle_publisher_request(
                        settings,
                        integration_registry,
                        &publisher_services,
                        orchestrator,
                        slots,
                        req,
                    )
                    .await
                    {
                        Ok(PublisherResponse::Stream {
                            mut response,
                            body,
                            mut params,
                        }) => {
                            // Streaming path: finalize headers, then stream body to client.
                            // TTFB happens at stream_to_client() — SSP bids are already
                            // in-flight in Fastly's native layer (dispatched before origin wait).
                            finalize_response(settings, geo_info.as_ref(), &mut response);
                            let mut streaming_body = response.stream_to_client();
                            // stream_publisher_body_async falls back to the sync path
                            // when no auction was dispatched (dispatched_auction is None).
                            let stream_result = stream_publisher_body_async(
                                body,
                                &mut streaming_body,
                                &mut params,
                                settings,
                                integration_registry,
                                orchestrator,
                                &publisher_services,
                            )
                            .await;
                            if let Err(e) = stream_result {
                                // Headers already committed. Log and abort — client
                                // sees a truncated response. Standard proxy behavior.
                                log::error!("Streaming processing failed: {e:?}");
                                drop(streaming_body);
                            } else if let Err(e) = streaming_body.finish() {
                                log::error!("Failed to finish streaming body: {e}");
                            }
                            // Response already sent via stream_to_client()
                            return None;
                        }
                        Ok(PublisherResponse::PassThrough { mut response, body }) => {
                            // Binary pass-through: reattach body and send via send_to_client().
                            // This preserves Content-Length and avoids chunked encoding overhead.
                            // Fastly streams the body from its internal buffer — no WASM
                            // memory buffering occurs.
                            response.set_body(body);
                            Ok(response)
                        }
                        Ok(PublisherResponse::Buffered(response)) => Ok(response),
                        Err(e) => {
                            log::error!("Failed to proxy to publisher origin: {:?}", e);
                            Err(e)
                        }
                    }
                }
                Err(e) => Err(e),
            }
        }
    };

    // Convert any errors to HTTP error responses
    let mut response = result.unwrap_or_else(|e| to_error_response(&e));

    finalize_response(settings, geo_info.as_ref(), &mut response);

    Some(response)
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
fn finalize_response(settings: &Settings, geo_info: Option<&GeoInfo>, response: &mut Response) {
    if let Some(geo) = geo_info {
        geo.set_response_headers(response);
    } else {
        response.set_header(HEADER_X_GEO_INFO_AVAILABLE, "false");
    }

    if let Ok(v) = ::std::env::var(ENV_FASTLY_SERVICE_VERSION) {
        response.set_header(HEADER_X_TS_VERSION, v);
    }
    if ::std::env::var(ENV_FASTLY_IS_STAGING).as_deref() == Ok("1") {
        response.set_header(HEADER_X_TS_ENV, "staging");
    }

    for (key, value) in &settings.response_headers {
        // Never overwrite a privacy-critical Cache-Control header (private, no-store, etc.)
        // that was set for per-user responses (HTML or page-bids).
        if **key == header::CACHE_CONTROL
            && response
                .get_header(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("private"))
        {
            continue;
        }
        response.set_header(key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastly::mime;

    #[test]
    fn ja4_debug_response_uses_plain_text_and_fallback_values() {
        let req = Request::get("https://example.com/_ts/debug/ja4");

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
            response.get_header_str(header::CACHE_CONTROL),
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
