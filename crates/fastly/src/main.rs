use fastly::http::Method;
use fastly::{Error, Request, Response};
use log_fastly::Logger;

use trusted_server_common::advertiser::handle_ad_request;
use trusted_server_common::gam::{
    handle_gam_asset, handle_gam_custom_url, handle_gam_golden_url, handle_gam_render,
    handle_gam_test, handle_gam_test_page, is_gam_asset_path,
};
use trusted_server_common::gdpr::{handle_consent_request, handle_data_subject_request};
use trusted_server_common::partners::handle_partner_asset;
use trusted_server_common::prebid::handle_prebid_test;
use trusted_server_common::prebid_proxy::{handle_prebid_auction, handle_prebid_cookie_sync};
use trusted_server_common::privacy::handle_privacy_policy;
use trusted_server_common::publisher::{
    handle_edgepubs_page, handle_main_page, handle_publisher_request,
};
use trusted_server_common::settings::Settings;
use trusted_server_common::settings_data::get_settings;
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
/// Checks if the EdgePubs feature is enabled in experimental settings.
fn is_edgepubs_enabled(settings: &Settings) -> bool {
    settings
        .experimental
        .as_ref()
        .is_some_and(|e| e.enable_edge_pub)
}

async fn route_request(settings: Settings, req: Request) -> Result<Response, Error> {
    log::info!(
        "FASTLY_SERVICE_VERSION: {}",
        ::std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_else(|_| String::new())
    );

    // Get path and method for routing
    let path = req.get_path();
    let method = req.get_method();
    let is_edgepubs_enabled = is_edgepubs_enabled(&settings);

    // Match known routes and handle them
    let result = match (method, path, is_edgepubs_enabled) {
        // Main application routes - handle '/' dynamically based on experimental flag
        (&Method::GET, "/", true) => handle_edgepubs_page(&settings, req),

        (&Method::GET, "/auburndao", _) => handle_main_page(&settings, req),
        (&Method::GET, "/ad-creative", true) => handle_ad_request(&settings, req),
        // Direct asset serving for partner domains (like auburndao.com approach)
        (&Method::GET, path, true) if is_partner_asset_path(path) => {
            handle_partner_asset(&settings, req).await
        }
        // GAM asset serving (separate from Equativ, checked after Equativ)
        (&Method::GET, path, true) if is_gam_asset_path(path) => {
            handle_gam_asset(&settings, req).await
        }
        (&Method::GET, "/prebid-test", _) => handle_prebid_test(&settings, req).await,

        // Prebid Server first-party auction endpoint
        (&Method::POST, "/openrtb2/auction", _) => handle_prebid_auction(&settings, req).await,
        // Prebid Server first-party cookie sync
        (&Method::POST, "/cookie_sync", _) => handle_prebid_cookie_sync(&settings, req).await,

        // GAM (Google Ad Manager) routes
        (&Method::GET, "/gam-test", true) => handle_gam_test(&settings, req).await,
        (&Method::GET, "/gam-golden-url", true) => handle_gam_golden_url(&settings, req).await,
        (&Method::POST, "/gam-test-custom-url", true) => {
            handle_gam_custom_url(&settings, req).await
        }
        (&Method::GET, "/gam-render", true) => handle_gam_render(&settings, req).await,
        (&Method::GET, "/gam-test-page", true) => handle_gam_test_page(&settings, req),
        // GDPR compliance routes
        (&Method::GET | &Method::POST, "/gdpr/consent", _) => {
            handle_consent_request(&settings, req)
        }
        (&Method::GET | &Method::DELETE, "/gdpr/data", _) => {
            handle_data_subject_request(&settings, req)
        }

        // Static content pages
        (&Method::GET, "/privacy-policy", _) => handle_privacy_policy(&settings, req),
        (&Method::GET, "/why-trusted-server", _) => handle_why_trusted_server(&settings, req),

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
