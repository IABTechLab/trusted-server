use error_stack::Report;
use fastly::http::Method;
use fastly::{Error, Request, Response};
use log_fastly::Logger;

use trusted_server_common::auction::endpoints::handle_auction;
use trusted_server_common::auction::{build_orchestrator, AuctionOrchestrator};
use trusted_server_common::auth::enforce_basic_auth;
use trusted_server_common::error::TrustedServerError;
use trusted_server_common::integrations::IntegrationRegistry;
use trusted_server_common::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_common::publisher::{apply_standard_response_headers, handle_tsjs_dynamic};
use trusted_server_common::request_signing::{
    handle_deactivate_key, handle_rotate_key, handle_trusted_server_discovery,
    handle_verify_signature,
};
use trusted_server_common::settings::Settings;
use trusted_server_common::settings_data::get_settings;

mod error;
use crate::error::to_error_response;

use trusted_server_common::publisher::RouteResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteTarget {
    TsjsDynamic,
    Discovery,
    VerifySignature,
    RotateKey,
    DeactivateKey,
    Auction,
    FirstPartyProxy,
    FirstPartyClick,
    FirstPartySign,
    FirstPartyProxyRebuild,
    Integration,
    PublisherProxy,
}

fn classify_route(
    method: &Method,
    path: &str,
    integration_registry: &IntegrationRegistry,
) -> RouteTarget {
    if path.starts_with("/static/tsjs=") && method == Method::GET {
        return RouteTarget::TsjsDynamic;
    }

    match (method, path) {
        (&Method::GET, "/.well-known/trusted-server.json") => RouteTarget::Discovery,
        (&Method::POST, "/verify-signature") => RouteTarget::VerifySignature,
        (&Method::POST, "/admin/keys/rotate") => RouteTarget::RotateKey,
        (&Method::POST, "/admin/keys/deactivate") => RouteTarget::DeactivateKey,
        (&Method::POST, "/auction") => RouteTarget::Auction,
        (&Method::GET, "/first-party/proxy") => RouteTarget::FirstPartyProxy,
        (&Method::GET, "/first-party/click") => RouteTarget::FirstPartyClick,
        (&Method::GET, "/first-party/sign") | (&Method::POST, "/first-party/sign") => {
            RouteTarget::FirstPartySign
        }
        (&Method::POST, "/first-party/proxy-rebuild") => RouteTarget::FirstPartyProxyRebuild,
        (m, p) if integration_registry.has_route(m, p) => RouteTarget::Integration,
        _ => RouteTarget::PublisherProxy,
    }
}

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
        apply_standard_response_headers(settings, &mut response);
        return Ok(RouteResult::Buffered(response));
    }

    // Get path and method for routing
    let path = req.get_path().to_string();
    let method = req.get_method().clone();
    let target = classify_route(&method, &path, integration_registry);
    if target == RouteTarget::PublisherProxy {
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
                apply_standard_response_headers(settings, &mut err_resp);
                return Ok(RouteResult::Buffered(err_resp));
            }
        }
    }

    // Match known routes and handle them
    let result = match target {
        RouteTarget::TsjsDynamic => handle_tsjs_dynamic(&req, integration_registry),
        RouteTarget::Discovery => handle_trusted_server_discovery(settings, req),
        RouteTarget::VerifySignature => handle_verify_signature(settings, req),
        RouteTarget::RotateKey => handle_rotate_key(settings, req),
        RouteTarget::DeactivateKey => handle_deactivate_key(settings, req),
        RouteTarget::Auction => handle_auction(settings, orchestrator, req).await,
        RouteTarget::FirstPartyProxy => handle_first_party_proxy(settings, req).await,
        RouteTarget::FirstPartyClick => handle_first_party_click(settings, req).await,
        RouteTarget::FirstPartySign => handle_first_party_proxy_sign(settings, req).await,
        RouteTarget::FirstPartyProxyRebuild => {
            handle_first_party_proxy_rebuild(settings, req).await
        }
        RouteTarget::Integration => integration_registry
            .handle_proxy(&method, &path, settings, req)
            .await
            .unwrap_or_else(|| {
                Err(Report::new(TrustedServerError::BadRequest {
                    message: format!("Unknown integration route: {path}"),
                }))
            }),
        RouteTarget::PublisherProxy => unreachable!(),
    };

    // Convert any errors to HTTP error responses
    let mut response = result.unwrap_or_else(|e| to_error_response(&e));
    apply_standard_response_headers(settings, &mut response);

    Ok(RouteResult::Buffered(response))
}

fn init_logger() {
    let logger = Logger::builder()
        .default_endpoint("tslog")
        .echo_stdout(true)
        .max_level(log::LevelFilter::Info)
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
