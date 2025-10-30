use fastly::http::Method;
use fastly::{Error, Request, Response};
use log_fastly::Logger;

use trusted_server_common::ad::{handle_server_ad, handle_server_ad_get};
use trusted_server_common::auth::enforce_basic_auth;
use trusted_server_common::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_common::publisher::{handle_publisher_request, handle_tsjs_dynamic};
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

    futures::executor::block_on(route_request(settings, req))
}

async fn route_request(settings: Settings, req: Request) -> Result<Response, Error> {
    log::info!(
        "FASTLY_SERVICE_VERSION: {}",
        ::std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_else(|_| String::new())
    );

    if let Some(response) = enforce_basic_auth(&settings, &req) {
        return Ok(response);
    }

    // Get path and method for routing
    let path = req.get_path();
    let method = req.get_method();

    // Match known routes and handle them
    let result = match (method, path) {
        // Serve the tsjs library
        (&Method::GET, path) if path.starts_with("/static/tsjs=") => {
            handle_tsjs_dynamic(&settings, req)
        }

        // tsjs endpoints
        (&Method::GET, "/first-party/ad") => handle_server_ad_get(&settings, req).await,
        (&Method::POST, "/third-party/ad") => handle_server_ad(&settings, req).await,
        (&Method::GET, "/first-party/proxy") => handle_first_party_proxy(&settings, req).await,
        (&Method::GET, "/first-party/click") => handle_first_party_click(&settings, req).await,
        (&Method::GET, "/first-party/sign") | (&Method::POST, "/first-party/sign") => {
            handle_first_party_proxy_sign(&settings, req).await
        }
        (&Method::POST, "/first-party/proxy-rebuild") => {
            handle_first_party_proxy_rebuild(&settings, req).await
        }

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
    let mut response = result.unwrap_or_else(to_error_response);

    // Add X-Robots-Tag header to prevent crawlers and indexers
    response.set_header("X-Robots-Tag", "noindex, nofollow");

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
