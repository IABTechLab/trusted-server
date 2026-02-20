use error_stack::Report;
use fastly::http::Method;
use fastly::{Error, Request, Response};
use log_fastly::Logger;

use trusted_server_common::auction::endpoints::handle_auction;
use trusted_server_common::auction::{build_orchestrator, AuctionOrchestrator};
use trusted_server_common::auth::enforce_basic_auth;
use trusted_server_common::constants::{
    ENV_FASTLY_IS_STAGING, ENV_FASTLY_SERVICE_VERSION, HEADER_X_TS_ENV, HEADER_X_TS_VERSION,
};
use trusted_server_common::error::TrustedServerError;
use trusted_server_common::integrations::IntegrationRegistry;
use trusted_server_common::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_common::publisher::handle_tsjs_dynamic;
use trusted_server_common::request_signing::{
    handle_deactivate_key, handle_rotate_key, handle_trusted_server_discovery,
    handle_verify_signature,
};
use trusted_server_common::settings::Settings;
use trusted_server_common::settings_data::get_settings;

mod error;
use crate::error::to_error_response;

use trusted_server_common::publisher::RouteResult;

fn main() {
    fastly::init();
    init_logger();
    let req = Request::from_client();

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
    let orchestrator = build_orchestrator(&settings);

    let integration_registry = match IntegrationRegistry::new(&settings) {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to create integration registry: {:?}", e);
            to_error_response(&e).send_to_client();
            return;
        }
    };

    match futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        req,
    )) {
        Ok(RouteResult::Buffered(resp)) => resp.send_to_client(),
        Ok(RouteResult::Streamed) => { /* already streamed */ }
        Err(e) => {
            log::error!("Request routing failed: {:?}", e);
            Response::from_status(fastly::http::StatusCode::INTERNAL_SERVER_ERROR)
                .with_body(format!("Internal Server Error: {}", e))
                .send_to_client();
        }
    }
}

async fn route_request(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    integration_registry: &IntegrationRegistry,
    req: Request,
) -> Result<RouteResult, Error> {
    log::debug!(
        "FASTLY_SERVICE_VERSION: {}",
        ::std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_else(|_| String::new())
    );

    if let Some(mut response) = enforce_basic_auth(settings, &req) {
        for (key, value) in &settings.response_headers {
            response.set_header(key, value);
        }
        return Ok(RouteResult::Buffered(response));
    }

    // Get path and method for routing
    let path = req.get_path().to_string();
    let method = req.get_method().clone();

    // Check if it's the publisher proxy fallback
    let is_publisher_proxy = match (method.clone(), path.as_str()) {
        (Method::GET, p) if p.starts_with("/static/tsjs=") => false,
        (Method::GET, "/.well-known/trusted-server.json") => false,
        (Method::POST, "/verify-signature") => false,
        (Method::POST, "/admin/keys/rotate") => false,
        (Method::POST, "/admin/keys/deactivate") => false,
        (Method::POST, "/auction") => false,
        (Method::GET, "/first-party/proxy") => false,
        (Method::GET, "/first-party/click") => false,
        (Method::GET, "/first-party/sign") | (Method::POST, "/first-party/sign") => false,
        (Method::POST, "/first-party/proxy-rebuild") => false,
        (m, p) if integration_registry.has_route(&m, p) => false,
        _ => true,
    };

    if is_publisher_proxy {
        log::info!(
            "No known route matched for path: {}, proxying to publisher origin",
            path
        );

        use trusted_server_common::publisher::handle_publisher_request_streaming;
        match handle_publisher_request_streaming(settings, integration_registry, req) {
            Ok(route_result) => return Ok(route_result),
            Err(e) => {
                log::error!("Failed to proxy to publisher origin: {:?}", e);
                let mut err_resp = to_error_response(&e);
                for (key, value) in &settings.response_headers {
                    err_resp.set_header(key, value);
                }
                return Ok(RouteResult::Buffered(err_resp));
            }
        }
    }

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

        _ => unreachable!(),
    };

    // Convert any errors to HTTP error responses
    let mut response = result.unwrap_or_else(|e| to_error_response(&e));

    if let Ok(v) = ::std::env::var(ENV_FASTLY_SERVICE_VERSION) {
        response.set_header(HEADER_X_TS_VERSION, v);
    }
    if ::std::env::var(ENV_FASTLY_IS_STAGING).as_deref() == Ok("1") {
        response.set_header(HEADER_X_TS_ENV, "staging");
    }

    for (key, value) in &settings.response_headers {
        response.set_header(key, value);
    }

    Ok(RouteResult::Buffered(response))
}

fn init_logger() {
    let logger = Logger::builder()
        .default_endpoint("tslog")
        .echo_stdout(true)
        .max_level(log::LevelFilter::Debug)
        .build()
        .expect("Failed to build Logger");

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
        .expect("Failed to initialize logger");
}
