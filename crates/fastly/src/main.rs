use fastly::http::{header, Method, StatusCode};
use fastly::{Error, Request, Response};
use log_fastly::Logger;

use trusted_server_common::advertiser::handle_ad_request;
use trusted_server_common::gam::{
    handle_gam_custom_url, handle_gam_golden_url, handle_gam_render, handle_gam_test,
};
use trusted_server_common::gdpr::{handle_consent_request, handle_data_subject_request};
use trusted_server_common::prebid::handle_prebid_test;
use trusted_server_common::privacy::handle_privacy_policy;
use trusted_server_common::publisher::handle_publisher_request;
use trusted_server_common::settings::Settings;
use trusted_server_common::settings_data::get_settings;
use trusted_server_common::templates::GAM_TEST_TEMPLATE;
use trusted_server_common::why::handle_why_trusted_server;

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

    futures::executor::block_on(route_request(settings, req))
}

/// Routes incoming requests to appropriate handlers.
///
/// This function implements the application's routing logic. It first checks
/// for known routes, and if none match, it proxies the request to the
/// publisher's origin server as a fallback.
async fn route_request(settings: Settings, req: Request) -> Result<Response, Error> {
    log::info!(
        "FASTLY_SERVICE_VERSION: {}",
        ::std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_else(|_| String::new())
    );

    // Get path and method for routing
    let path = req.get_path();
    let method = req.get_method();

    // Match known routes and handle them
    let result = match (method, path) {
        // Main application routes
        (&Method::GET, "/ad-creative") => handle_ad_request(&settings, req),
        (&Method::GET, "/prebid-test") => handle_prebid_test(&settings, req).await,

        // GAM (Google Ad Manager) routes
        (&Method::GET, "/gam-test") => handle_gam_test(&settings, req).await,
        (&Method::GET, "/gam-golden-url") => handle_gam_golden_url(&settings, req).await,
        (&Method::POST, "/gam-test-custom-url") => handle_gam_custom_url(&settings, req).await,
        (&Method::GET, "/gam-render") => handle_gam_render(&settings, req).await,
        (&Method::GET, "/gam-test-page") => Ok(Response::from_status(StatusCode::OK)
            .with_body(GAM_TEST_TEMPLATE)
            .with_header(header::CONTENT_TYPE, "text/html")
            .with_header("x-compress-hint", "on")),

        // GDPR compliance routes
        (&Method::GET | &Method::POST, "/gdpr/consent") => handle_consent_request(&settings, req),
        (&Method::GET | &Method::DELETE, "/gdpr/data") => {
            handle_data_subject_request(&settings, req)
        }

        // Static content pages
        (&Method::GET, "/privacy-policy") => handle_privacy_policy(&settings, req),
        (&Method::GET, "/why-trusted-server") => handle_why_trusted_server(&settings, req),

        // No known route matched, proxy to publisher origin as fallback
        _ => {
            log::info!(
                "No known route matched for path: {}, proxying to publisher origin",
                path
            );

            match handle_publisher_request(&settings, req) {
                Ok(response) => Ok(response),
                Err(e) => {
                    log::error!("Failed to proxy to publisher origin: {:?}", e);
                    Err(e)
                }
            }
        }
    };

    // Convert any errors to HTTP error responses
    result.map_or_else(|e| Ok(to_error_response(e)), Ok)
}

fn init_logger() {
    let logger = Logger::builder()
        .default_endpoint("tslog")
        // .echo_stdout(true)
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
