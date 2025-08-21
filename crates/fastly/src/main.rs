use fastly::http::{header, Method, StatusCode};
use fastly::{Error, Request, Response};
use log_fastly::Logger;

mod error;
use crate::error::to_error_response;

use trusted_server_common::advertiser::handle_ad_request;
use trusted_server_common::constants::HEADER_X_COMPRESS_HINT;
use trusted_server_common::gam::{
    handle_gam_asset, handle_gam_custom_url, handle_gam_golden_url, handle_gam_render,
    handle_gam_test, is_gam_asset_path,
};
use trusted_server_common::gdpr::{handle_consent_request, handle_data_subject_request};
use trusted_server_common::partners::handle_partner_asset;
use trusted_server_common::prebid::handle_prebid_test;
use trusted_server_common::privacy::handle_privacy_policy;
use trusted_server_common::publisher::{handle_edgepubs_page, handle_main_page};
use trusted_server_common::settings::Settings;
use trusted_server_common::settings_data::get_settings;
use trusted_server_common::templates::GAM_TEST_TEMPLATE;
use trusted_server_common::why::handle_why_trusted_server;
use trusted_server_common::didomi::DidomiProxy;


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

    futures::executor::block_on(route_request(settings, req))
}

/// Routes incoming requests to appropriate handlers.
///
/// This function implements the application's routing logic, matching HTTP methods
/// and paths to their corresponding handler functions.
async fn route_request(settings: Settings, req: Request) -> Result<Response, Error> {
    log::info!(
        "FASTLY_SERVICE_VERSION: {}",
        std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_else(|_| String::new())
    );

    let result = match (req.get_method(), req.get_path()) {
        // Main application routes
        (&Method::GET, "/") => handle_edgepubs_page(&settings, req),
        (&Method::GET, "/auburndao") => handle_main_page(&settings, req),
        (&Method::GET, "/ad-creative") => handle_ad_request(&settings, req),
        // Direct asset serving for partner domains (like auburndao.com approach)
        (&Method::GET, path) if is_partner_asset_path(path) => {
            handle_partner_asset(&settings, req).await
        }
        // GAM asset serving (separate from Equativ, checked after Equativ)
        (&Method::GET, path) if is_gam_asset_path(path) => handle_gam_asset(&settings, req).await,
        (&Method::GET, "/prebid-test") => handle_prebid_test(&settings, req).await,
        (&Method::GET, "/gam-test") => handle_gam_test(&settings, req).await,
        (&Method::GET, "/gam-golden-url") => handle_gam_golden_url(&settings, req).await,
        (&Method::POST, "/gam-test-custom-url") => handle_gam_custom_url(&settings, req).await,
        (&Method::GET, "/gam-render") => handle_gam_render(&settings, req).await,
        (&Method::GET, "/gam-test-page") => Ok(Response::from_status(StatusCode::OK)
            .with_body(GAM_TEST_TEMPLATE)
            .with_header(header::CONTENT_TYPE, "text/html")
            .with_header("x-compress-hint", "on")),
        (&Method::GET | &Method::POST, "/gdpr/consent") => handle_consent_request(&settings, req),
        (&Method::GET | &Method::DELETE, "/gdpr/data") => {
            handle_data_subject_request(&settings, req)
        }
        (&Method::GET, "/privacy-policy") => handle_privacy_policy(&settings, req),
        (&Method::GET, "/why-trusted-server") => handle_why_trusted_server(&settings, req),

        // Didomi CMP routes
        (_, path) if path.starts_with("/consent/") => {
            DidomiProxy::handle_consent_request(&settings, req).await
        }

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

/// Check if the path is for an Equativ asset that should be served directly (like auburndao.com)
fn is_partner_asset_path(path: &str) -> bool {
    // Only handle Equativ/Smart AdServer assets for now
    path.contains("/diff/") ||          // Equativ assets
    path.ends_with(".png") ||           // Images
    path.ends_with(".jpg") ||           // Images
    path.ends_with(".gif") // Images
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
