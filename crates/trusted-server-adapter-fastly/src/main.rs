use error_stack::Report;
use fastly::http::Method;
use fastly::{Request, Response};
use log_fastly::Logger;

use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{build_orchestrator, AuctionOrchestrator};
use trusted_server_core::auth::enforce_basic_auth;
use trusted_server_core::constants::{
    ENV_FASTLY_IS_STAGING, ENV_FASTLY_SERVICE_VERSION, HEADER_X_GEO_INFO_AVAILABLE,
    HEADER_X_TS_ENV, HEADER_X_TS_VERSION,
};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::http_util::sanitize_forwarded_headers;
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    handle_publisher_request, handle_tsjs_dynamic, stream_publisher_body, PublisherResponse,
};
use trusted_server_core::request_signing::{
    handle_deactivate_key, handle_rotate_key, handle_trusted_server_discovery,
    handle_verify_signature,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::settings_data::get_settings;

mod error;
use crate::error::to_error_response;

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

    // route_request may send the response directly (streaming path) or
    // return it for us to send (buffered path).
    if let Some(response) = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        req,
    )) {
        response.send_to_client();
    }
}

async fn route_request(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    integration_registry: &IntegrationRegistry,
    mut req: Request,
) -> Option<Response> {
    // Strip client-spoofable forwarded headers at the edge.
    // On Fastly this service IS the first proxy — these headers from
    // clients are untrusted and can hijack URL rewriting (see #409).
    sanitize_forwarded_headers(&mut req);

    // Extract geo info before auth check or routing consumes the request
    let geo_info = GeoInfo::from_request(&req);

    // `get_settings()` should already have rejected invalid handler regexes.
    // Keep this fallback so manually-constructed or otherwise unprepared
    // settings still become an error response instead of panicking.
    match enforce_basic_auth(settings, &req) {
        Ok(Some(mut response)) => {
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
            handle_trusted_server_discovery(settings, req)
        }

        // Signature verification endpoint
        (Method::POST, "/verify-signature") => handle_verify_signature(settings, req),

        // Key rotation admin endpoints
        // Keep in sync with Settings::ADMIN_ENDPOINTS in crates/trusted-server-core/src/settings.rs
        (Method::POST, "/admin/keys/rotate") => handle_rotate_key(settings, req),
        (Method::POST, "/admin/keys/deactivate") => handle_deactivate_key(settings, req),

        // Unified auction endpoint (returns creative HTML inline)
        (Method::POST, "/auction") => handle_auction(settings, orchestrator, req).await,

        // tsjs endpoints
        (Method::GET, "/first-party/proxy") => handle_first_party_proxy(settings, req).await,
        (Method::GET, "/first-party/click") => handle_first_party_click(settings, req).await,
        (Method::GET, "/first-party/sign") | (Method::POST, "/first-party/sign") => {
            handle_first_party_proxy_sign(settings, req).await
        }
        (Method::POST, "/first-party/proxy-rebuild") => {
            handle_first_party_proxy_rebuild(settings, req).await
        }
        (m, path) if integration_registry.has_route(&m, path) => integration_registry
            .handle_proxy(&m, path, settings, req)
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

            match handle_publisher_request(settings, integration_registry, req) {
                Ok(PublisherResponse::Stream {
                    mut response,
                    body,
                    params,
                }) => {
                    // Streaming path: finalize headers, then stream body to client.
                    finalize_response(settings, geo_info.as_ref(), &mut response);
                    let mut streaming_body = response.stream_to_client();
                    if let Err(e) = stream_publisher_body(
                        body,
                        &mut streaming_body,
                        &params,
                        settings,
                        integration_registry,
                    ) {
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
                Ok(PublisherResponse::Buffered(response)) => Ok(response),
                Err(e) => {
                    log::error!("Failed to proxy to publisher origin: {:?}", e);
                    Err(e)
                }
            }
        }
    };

    // Convert any errors to HTTP error responses
    let mut response = result.unwrap_or_else(|e| to_error_response(&e));

    finalize_response(settings, geo_info.as_ref(), &mut response);

    Some(response)
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
        response.set_header(key, value);
    }
}

fn init_logger() {
    let logger = Logger::builder()
        .default_endpoint("tslog")
        .echo_stdout(true)
        .max_level(log::LevelFilter::Info)
        .build()
        .expect("should build Logger");

    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} {} [{}] {}",
                chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                record.level(),
                record
                    .target()
                    .split("::")
                    .last()
                    .unwrap_or(record.target()),
                message
            ))
        })
        .chain(Box::new(logger) as Box<dyn log::Log>)
        .apply()
        .expect("should initialize logger");
}
