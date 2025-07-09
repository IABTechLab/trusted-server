use fastly::http::{header, Method, StatusCode};
use fastly::{Error, Request, Response};
use log::LevelFilter::Info;

mod error;
use crate::error::to_error_response;

use trusted_server_common::advertiser::handle_ad_request;
use trusted_server_common::constants::HEADER_X_COMPRESS_HINT;
use trusted_server_common::gdpr::{handle_consent_request, handle_data_subject_request};
use trusted_server_common::prebid::handle_prebid_test;
use trusted_server_common::privacy::handle_privacy_policy;
use trusted_server_common::publisher::handle_main_page;
use trusted_server_common::settings::Settings;
use trusted_server_common::why::handle_why_trusted_server;

#[fastly::main]
fn main(req: Request) -> Result<Response, Error> {
    log_fastly::init_simple("mylogs", Info);

    let settings = match Settings::new() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to load settings: {:?}", e);
            return Ok(to_error_response(e));
        }
    };
    log::info!("Settings {settings:?}");

    futures::executor::block_on(route_request(settings, req))
}

/// Routes incoming requests to appropriate handlers.
///
/// This function implements the application's routing logic, matching HTTP methods
/// and paths to their corresponding handler functions.
async fn route_request(settings: Settings, req: Request) -> Result<Response, Error> {
    log::info!(
        "FASTLY_SERVICE_VERSION: {}",
        ::std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_else(|_| String::new())
    );

    let result = match (req.get_method(), req.get_path()) {
        // Main application routes
        (&Method::GET, "/") => handle_main_page(&settings, req),
        (&Method::GET, "/ad-creative") => handle_ad_request(&settings, req),
        (&Method::GET, "/prebid-test") => handle_prebid_test(&settings, req).await,
        
        // GDPR compliance routes
        (&Method::GET | &Method::POST, "/gdpr/consent") => handle_consent_request(&settings, req),
        (&Method::GET | &Method::DELETE, "/gdpr/data") => handle_data_subject_request(&settings, req),
        
        // Static content pages
        (&Method::GET, "/privacy-policy") => handle_privacy_policy(&settings, req),
        (&Method::GET, "/why-trusted-server") => handle_why_trusted_server(&settings, req),
        
        // Catch-all 404 handler
        _ => return Ok(not_found_response()),
    };

    // Convert any errors to HTTP error responses
    result.map_or_else(|e| Ok(to_error_response(e)), Ok)
}

/// Creates a standard 404 Not Found response.
fn not_found_response() -> Response {
    Response::from_status(StatusCode::NOT_FOUND)
        .with_body("Not Found")
        .with_header(header::CONTENT_TYPE, "text/plain")
        .with_header(HEADER_X_COMPRESS_HINT, "on")
}
