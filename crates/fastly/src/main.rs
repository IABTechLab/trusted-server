use error_stack::Report;
use fastly::http::Method;
use fastly::{Error, Request, Response};
use log_fastly::Logger;

use trusted_server_common::auth::enforce_basic_auth;
use trusted_server_common::error::TrustedServerError;
use trusted_server_common::integrations::IntegrationRegistry;
use trusted_server_common::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_common::publisher::{handle_publisher_request, handle_tsjs_dynamic};
use trusted_server_common::request_signing::{
    handle_deactivate_key, handle_rotate_key, handle_trusted_server_discovery,
    handle_verify_signature,
};
use trusted_server_common::settings::Settings;
use trusted_server_common::settings_data::get_settings;

mod error;
use crate::error::to_error_response;

#[fastly::main]
fn main(req: Request) -> Result<Response, Error> {
    init_logger();

    let settings = match get_settings() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to load settings: {:?}", e);
            return Ok(to_error_response(e));
        }
    };
    log::info!("Settings {settings:?}");
    let integration_registry = IntegrationRegistry::new(&settings);

    futures::executor::block_on(route_request(settings, integration_registry, req))
}

async fn route_request(
    settings: Settings,
    integration_registry: IntegrationRegistry,
    req: Request,
) -> Result<Response, Error> {
    log::info!(
        "FASTLY_SERVICE_VERSION: {}",
        ::std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_else(|_| String::new())
    );

    if let Some(response) = enforce_basic_auth(&settings, &req) {
        return Ok(response);
    }

    // Get path and method for routing
    let path = req.get_path().to_string();
    let method = req.get_method().clone();

    // Match known routes and handle them
    let result = match (method, path.as_str()) {
        // Serve the tsjs library
        (Method::GET, path) if path.starts_with("/static/tsjs=") => handle_tsjs_dynamic(req),

        // Discovery endpoint for trusted-server capabilities and JWKS
        (Method::GET, "/.well-known/trusted-server.json") => {
            handle_trusted_server_discovery(&settings, req)
        }

        // Signature verification endpoint
        (Method::POST, "/verify-signature") => handle_verify_signature(&settings, req),

        // Key rotation admin endpoints
        (Method::POST, "/admin/keys/rotate") => handle_rotate_key(&settings, req),
        (Method::POST, "/admin/keys/deactivate") => handle_deactivate_key(&settings, req),

        // tsjs endpoints
        (Method::GET, "/first-party/proxy") => handle_first_party_proxy(&settings, req).await,
        (Method::GET, "/first-party/click") => handle_first_party_click(&settings, req).await,
        (Method::GET, "/first-party/sign") | (Method::POST, "/first-party/sign") => {
            handle_first_party_proxy_sign(&settings, req).await
        }
        (Method::POST, "/first-party/proxy-rebuild") => {
            handle_first_party_proxy_rebuild(&settings, req).await
        }
        (m, path) if integration_registry.has_route(&m, path) => integration_registry
            .handle_proxy(&m, path, &settings, req)
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

            match handle_publisher_request(&settings, &integration_registry, req) {
                Ok(response) => Ok(response),
                Err(e) => {
                    log::error!("Failed to proxy to publisher origin: {:?}", e);
                    Err(e)
                }
            }
        }
    };

    // Convert any errors to HTTP error responses
    let mut response = result.unwrap_or_else(to_error_response);

    for (key, value) in &settings.response_headers {
        response.set_header(key, value);
    }

    Ok(response)
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
                "{}  {} {}",
                chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                record.level(),
                message
            ))
        })
        .chain(Box::new(logger) as Box<dyn log::Log>)
        .apply()
        .expect("Failed to initialize logger");
}
