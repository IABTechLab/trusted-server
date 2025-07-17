use std::env;

use fastly::geo::geo_lookup;
use fastly::http::{header, Method, StatusCode};
use fastly::KVStore;
use fastly::{Error, Request, Response};
use log::LevelFilter::Info;
use log_fastly::Logger;
use serde_json::json;

mod error;
use crate::error::to_error_response;

use trusted_server_common::constants::{
    HEADER_SYNTHETIC_FRESH, HEADER_SYNTHETIC_TRUSTED_SERVER, HEADER_X_COMPRESS_HINT,
    HEADER_X_CONSENT_ADVERTISING, HEADER_X_FORWARDED_FOR, HEADER_X_GEO_CITY,
    HEADER_X_GEO_CONTINENT, HEADER_X_GEO_COORDINATES, HEADER_X_GEO_COUNTRY,
    HEADER_X_GEO_INFO_AVAILABLE, HEADER_X_GEO_METRO_CODE,
};
use trusted_server_common::cookies::create_synthetic_cookie;
use trusted_server_common::gam::{
    handle_gam_custom_url, handle_gam_golden_url, handle_gam_render, handle_gam_test,
};
// Note: TrustedServerError is used internally by the common crate
use trusted_server_common::gdpr::{
    get_consent_from_request, handle_consent_request, handle_data_subject_request, GdprConsent,
};
use trusted_server_common::models::AdResponse;
use trusted_server_common::prebid::PrebidRequest;
use trusted_server_common::privacy::PRIVACY_TEMPLATE;
use trusted_server_common::settings::Settings;
use trusted_server_common::synthetic::{generate_synthetic_id, get_or_generate_synthetic_id};
use trusted_server_common::templates::{GAM_TEST_TEMPLATE, HTML_TEMPLATE};
use trusted_server_common::why::WHY_TEMPLATE;

#[fastly::main]
fn main(req: Request) -> Result<Response, Error> {
    init_logger();

    // Print Settings only once at the beginning
    let settings = match Settings::new() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to load settings: {:?}", e);
            return Ok(to_error_response(e));
        }
    };
    log::info!("Settings {settings:?}");
    // Print User IP address immediately after Fastly Service Version
    let client_ip = req
        .get_client_ip_addr()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "Unknown".to_string());
    log::info!("User IP: {}", client_ip);

    futures::executor::block_on(async {
        log::info!(
            "FASTLY_SERVICE_VERSION: {}",
            std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_else(|_| String::new())
        );

        match (req.get_method(), req.get_path()) {
            (&Method::GET, "/") => handle_main_page(&settings, req),
            (&Method::GET, "/ad-creative") => handle_ad_request(&settings, req),
            (&Method::GET, "/prebid-test") => handle_prebid_test(&settings, req).await,
            (&Method::GET, "/gam-test") => handle_gam_test(&settings, req).await,
            (&Method::GET, "/gam-golden-url") => handle_gam_golden_url(&settings, req).await,
            (&Method::POST, "/gam-test-custom-url") => handle_gam_custom_url(&settings, req).await,
            (&Method::GET, "/gam-render") => handle_gam_render(&settings, req).await,
            (&Method::GET, "/gam-test-page") => Ok(Response::from_status(StatusCode::OK)
                .with_body(GAM_TEST_TEMPLATE)
                .with_header(header::CONTENT_TYPE, "text/html")
                .with_header("x-compress-hint", "on")),
            (&Method::GET, "/gdpr/consent") => handle_consent_request(&settings, req),
            (&Method::POST, "/gdpr/consent") => handle_consent_request(&settings, req),
            (&Method::GET, "/gdpr/data") => handle_data_subject_request(&settings, req),
            (&Method::DELETE, "/gdpr/data") => handle_data_subject_request(&settings, req),
            (&Method::GET, "/privacy-policy") => Ok(Response::from_status(StatusCode::OK)
                .with_body(PRIVACY_TEMPLATE)
                .with_header(header::CONTENT_TYPE, "text/html")
                .with_header(HEADER_X_COMPRESS_HINT, "on")),
            (&Method::GET, "/why-trusted-server") => Ok(Response::from_status(StatusCode::OK)
                .with_body(WHY_TEMPLATE)
                .with_header(header::CONTENT_TYPE, "text/html")
                .with_header(HEADER_X_COMPRESS_HINT, "on")),
            _ => Ok(Response::from_status(StatusCode::NOT_FOUND)
                .with_body("Not Found")
                .with_header(header::CONTENT_TYPE, "text/plain")
                .with_header(HEADER_X_COMPRESS_HINT, "on")),
        }
    })
}

fn get_dma_code(req: &mut Request) -> Option<String> {
    // Debug: Check if we're running in Fastly environment
    log::info!("Fastly Environment Check:");
    log::info!(
        "  FASTLY_POP: {}",
        std::env::var("FASTLY_POP").unwrap_or_else(|_| "not in Fastly".to_string())
    );
    log::info!(
        "  FASTLY_REGION: {}",
        std::env::var("FASTLY_REGION").unwrap_or_else(|_| "not in Fastly".to_string())
    );

    // Get detailed geo information using geo_lookup
    if let Some(geo) = req.get_client_ip_addr().and_then(geo_lookup) {
        log::info!("Geo Information Found:");

        // Set all available geo information in headers
        let city = geo.city();
        req.set_header(HEADER_X_GEO_CITY, city);
        log::info!("  City: {}", city);

        let country = geo.country_code();
        req.set_header(HEADER_X_GEO_COUNTRY, country);
        log::info!("  Country: {}", country);

        req.set_header(HEADER_X_GEO_CONTINENT, format!("{:?}", geo.continent()));
        log::info!("  Continent: {:?}", geo.continent());

        req.set_header(
            HEADER_X_GEO_COORDINATES,
            format!("{},{}", geo.latitude(), geo.longitude()),
        );
        log::info!("  Location: ({}, {})", geo.latitude(), geo.longitude());

        // Get and set the metro code (DMA)
        let metro_code = geo.metro_code();
        req.set_header(HEADER_X_GEO_METRO_CODE, metro_code.to_string());
        log::info!("Found DMA/Metro code: {}", metro_code);
        return Some(metro_code.to_string());
    } else {
        log::info!("No geo information available for the request");
        req.set_header(HEADER_X_GEO_INFO_AVAILABLE, "false");
    }

    // If no metro code is found, log all request headers for debugging
    log::info!("No DMA/Metro code found. All request headers:");
    for (name, value) in req.get_headers() {
        log::info!("  {}: {:?}", name, value);
    }

    None
}

/// Handles the main page request.
///
/// Serves the main page with synthetic ID generation and ad integration.
///
/// # Errors
///
/// Returns a Fastly [`Error`] if response creation fails.
fn handle_main_page(settings: &Settings, mut req: Request) -> Result<Response, Error> {
    log::info!(
        "Using ad_partner_url: {}, counter_store: {}",
        settings.ad_server.ad_partner_url,
        settings.synthetic.counter_store,
    );

    // Add DMA code check to main page as well
    let dma_code = get_dma_code(&mut req);
    log::info!("Main page - DMA Code: {:?}", dma_code);

    // Check GDPR consent before proceeding
    let consent = match get_consent_from_request(&req) {
        Some(c) => c,
        None => {
            log::debug!("No GDPR consent found, using default");
            GdprConsent::default()
        }
    };
    if !consent.functional {
        // Return a version of the page without tracking
        return Ok(Response::from_status(StatusCode::OK)
            .with_body(
                HTML_TEMPLATE.replace("fetch('/prebid-test')", "console.log('Tracking disabled')"),
            )
            .with_header(header::CONTENT_TYPE, "text/html")
            .with_header(header::CACHE_CONTROL, "no-store, private"));
    }

    // Calculate fresh ID first using the synthetic module
    let fresh_id = match generate_synthetic_id(settings, &req) {
        Ok(id) => id,
        Err(e) => return Ok(to_error_response(e)),
    };

    // Check for existing Trusted Server ID in this specific order:
    // 1. X-Synthetic-Trusted-Server header
    // 2. Cookie
    // 3. Fall back to fresh ID
    let synthetic_id = match get_or_generate_synthetic_id(settings, &req) {
        Ok(id) => id,
        Err(e) => return Ok(to_error_response(e)),
    };

    log::info!(
        "Existing Trusted Server header: {:?}",
        req.get_header(HEADER_SYNTHETIC_TRUSTED_SERVER)
    );
    log::info!("Generated Fresh ID: {}", &fresh_id);
    log::info!("Using Trusted Server ID: {}", synthetic_id);

    // Create response with the main page HTML
    let mut response = Response::from_status(StatusCode::OK)
        .with_body(HTML_TEMPLATE)
        .with_header(header::CONTENT_TYPE, "text/html")
        .with_header(HEADER_SYNTHETIC_FRESH, fresh_id.as_str()) // Fresh ID always changes
        .with_header(HEADER_SYNTHETIC_TRUSTED_SERVER, &synthetic_id) // Trusted Server ID remains stable
        .with_header(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            "X-Geo-City, X-Geo-Country, X-Geo-Continent, X-Geo-Coordinates, X-Geo-Metro-Code, X-Geo-Info-Available"
        )
        .with_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .with_header("x-compress-hint", "on");

    // Copy geo headers from request to response
    for header_name in &[
        "X-Geo-City",
        "X-Geo-Country",
        "X-Geo-Continent",
        "X-Geo-Coordinates",
        "X-Geo-Metro-Code",
        "X-Geo-Info-Available",
    ] {
        if let Some(value) = req.get_header(*header_name) {
            response.set_header(*header_name, value);
        }
    }

    // Only set cookies if we have consent
    if consent.functional {
        response.set_header(
            header::SET_COOKIE,
            create_synthetic_cookie(settings, &synthetic_id),
        );
    }

    // Debug: Print all request headers
    log::info!("All Request Headers:");
    for (name, value) in req.get_headers() {
        log::info!("{}: {:?}", name, value);
    }

    // Debug: Print the response headers
    log::info!("Response Headers:");
    for (name, value) in response.get_headers() {
        log::info!("{}: {:?}", name, value);
    }

    // Prevent caching
    response.set_header(header::CACHE_CONTROL, "no-store, private");

    Ok(response)
}

/// Handles ad creative requests.
///
/// Processes ad requests with synthetic ID and consent checking.
///
/// # Errors
///
/// Returns a Fastly [`Error`] if response creation fails.
fn handle_ad_request(settings: &Settings, mut req: Request) -> Result<Response, Error> {
    // Check GDPR consent to determine if we should serve personalized or non-personalized ads
    let _consent = match get_consent_from_request(&req) {
        Some(c) => c,
        None => {
            log::debug!("No GDPR consent found in ad request, using default");
            GdprConsent::default()
        }
    };
    let advertising_consent = req
        .get_header(HEADER_X_CONSENT_ADVERTISING)
        .and_then(|h| h.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false);

    // Add DMA code extraction
    let dma_code = get_dma_code(&mut req);

    log::info!("Client location - DMA Code: {:?}", dma_code);

    // Log headers for debugging
    let client_ip = req
        .get_client_ip_addr()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "Unknown".to_string());
    let x_forwarded_for = req
        .get_header(HEADER_X_FORWARDED_FOR)
        .map(|h| h.to_str().unwrap_or("Unknown"));

    log::info!("Client IP: {}", client_ip);
    log::info!("X-Forwarded-For: {}", x_forwarded_for.unwrap_or("None"));
    log::info!("Advertising consent: {}", advertising_consent);

    // Generate synthetic ID only if we have consent
    let synthetic_id = if advertising_consent {
        match generate_synthetic_id(settings, &req) {
            Ok(id) => id,
            Err(e) => return Ok(to_error_response(e)),
        }
    } else {
        // Use a generic ID for non-personalized ads
        "non-personalized".to_string()
    };

    // Only track visits if we have consent
    if advertising_consent {
        // Increment visit counter in KV store
        log::info!("Opening KV store: {}", settings.synthetic.counter_store);
        if let Ok(Some(store)) = KVStore::open(settings.synthetic.counter_store.as_str()) {
            log::info!("Fetching current count for synthetic ID: {}", synthetic_id);
            let current_count: i32 = store
                .lookup(&synthetic_id)
                .map(|mut val| match String::from_utf8(val.take_body_bytes()) {
                    Ok(s) => {
                        log::info!("Value from KV store: {}", s);
                        Some(s)
                    }
                    Err(e) => {
                        log::error!("Error converting bytes to string: {}", e);
                        None
                    }
                })
                .map(|opt_s| {
                    log::info!("Parsing string value: {:?}", opt_s);
                    opt_s.and_then(|s| s.parse().ok())
                })
                .unwrap_or_else(|_| {
                    log::info!("No existing count found, starting at 0");
                    None
                })
                .unwrap_or(0);

            let new_count = current_count + 1;
            log::info!("Incrementing count from {} to {}", current_count, new_count);

            if let Err(e) = store.insert(&synthetic_id, new_count.to_string().as_bytes()) {
                log::error!("Error updating KV store: {:?}", e);
            }
        }
    }

    // Modify the ad server URL construction to include DMA code if available
    let ad_server_url = if advertising_consent {
        let mut url = settings
            .ad_server
            .sync_url
            .replace("{{synthetic_id}}", &synthetic_id);
        if let Some(dma) = dma_code {
            url = format!("{}&dma={}", url, dma);
        }
        url
    } else {
        // Use a different URL or parameter for non-personalized ads
        settings
            .ad_server
            .sync_url
            .replace("{{synthetic_id}}", "non-personalized")
    };

    log::info!("Sending request to backend: {}", ad_server_url);

    // Add header logging here
    let mut ad_req = Request::get(ad_server_url);

    // Add consent information to the ad request
    ad_req.set_header(
        HEADER_X_CONSENT_ADVERTISING,
        if advertising_consent { "true" } else { "false" },
    );

    log::info!("Request headers to Equativ:");
    for (name, value) in ad_req.get_headers() {
        log::info!("  {}: {:?}", name, value);
    }

    match ad_req.send(settings.ad_server.ad_partner_url.as_str()) {
        Ok(mut res) => {
            log::info!(
                "Received response from backend with status: {}",
                res.get_status()
            );

            // Extract Fastly PoP from the Compute environment
            let fastly_pop = env::var("FASTLY_POP").unwrap_or_else(|_| "unknown".to_string());
            let fastly_cache_generation =
                env::var("FASTLY_CACHE_GENERATION").unwrap_or_else(|_| "unknown".to_string());
            let fastly_customer_id =
                env::var("FASTLY_CUSTOMER_ID").unwrap_or_else(|_| "unknown".to_string());
            let fastly_hostname =
                env::var("FASTLY_HOSTNAME").unwrap_or_else(|_| "unknown".to_string());
            let fastly_region = env::var("FASTLY_REGION").unwrap_or_else(|_| "unknown".to_string());
            let fastly_service_id =
                env::var("FASTLY_SERVICE_ID").unwrap_or_else(|_| "unknown".to_string());
            let fastly_trace_id =
                env::var("FASTLY_TRACE_ID").unwrap_or_else(|_| "unknown".to_string());

            log::info!("Fastly POP: {}", fastly_pop);
            log::info!("Fastly Compute Variables:");
            log::info!("  - FASTLY_CACHE_GENERATION: {}", fastly_cache_generation);
            log::info!("  - FASTLY_CUSTOMER_ID: {}", fastly_customer_id);
            log::info!("  - FASTLY_HOSTNAME: {}", fastly_hostname);
            log::info!("  - FASTLY_POP: {}", fastly_pop);
            log::info!("  - FASTLY_REGION: {}", fastly_region);
            log::info!("  - FASTLY_SERVICE_ID: {}", fastly_service_id);
            //log::info!("  - FASTLY_SERVICE_VERSION: {}", fastly_service_version);
            log::info!("  - FASTLY_TRACE_ID: {}", fastly_trace_id);

            // Log all response headers
            log::info!("Response headers from Equativ:");
            for (name, value) in res.get_headers() {
                log::info!("  {}: {:?}", name, value);
            }

            if res.get_status().is_success() {
                let body = res.take_body_str();
                log::info!("Backend response body: {}", body);

                // Parse the JSON response and extract opid
                if let Ok(ad_response) = serde_json::from_str::<AdResponse>(&body) {
                    // Look for the callback with type "impression"
                    if let Some(callback) = ad_response
                        .callbacks
                        .iter()
                        .find(|c| c.callback_type == "impression")
                    {
                        // Extract opid from the URL
                        if let Some(opid) = callback
                            .url
                            .split('&')
                            .find(|&param| param.starts_with("opid="))
                            .and_then(|param| param.split('=').nth(1))
                        {
                            log::info!("Found opid: {}", opid);

                            // Store in opid KV store
                            log::info!(
                                "Attempting to open KV store: {}",
                                settings.synthetic.opid_store
                            );
                            match KVStore::open(settings.synthetic.opid_store.as_str()) {
                                Ok(Some(store)) => {
                                    log::info!("Successfully opened KV store");
                                    match store.insert(&synthetic_id, opid.as_bytes()) {
                                        Ok(_) => log::info!(
                                            "Successfully stored opid {} for synthetic ID: {}",
                                            opid,
                                            synthetic_id
                                        ),
                                        Err(e) => {
                                            log::error!("Error storing opid in KV store: {:?}", e)
                                        }
                                    }
                                }
                                Ok(None) => {
                                    log::warn!(
                                        "KV store returned None: {}",
                                        settings.synthetic.opid_store
                                    );
                                }
                                Err(e) => {
                                    log::error!(
                                        "Error opening KV store '{}': {:?}",
                                        settings.synthetic.opid_store,
                                        e
                                    );
                                }
                            };
                        }
                    }
                }

                // Return the JSON response with CORS headers
                let mut response = Response::from_status(StatusCode::OK)
                    .with_header(header::CONTENT_TYPE, "application/json")
                    .with_header(header::CACHE_CONTROL, "no-store, private")
                    .with_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                    .with_header(
                        header::ACCESS_CONTROL_EXPOSE_HEADERS,
                        "X-Geo-City, X-Geo-Country, X-Geo-Continent, X-Geo-Coordinates, X-Geo-Metro-Code, X-Geo-Info-Available"
                    )
                    .with_header(HEADER_X_COMPRESS_HINT, "on")
                    .with_body(body);

                // Copy geo headers from request to response
                for header_name in &[
                    HEADER_X_GEO_CITY,
                    HEADER_X_GEO_COUNTRY,
                    HEADER_X_GEO_CONTINENT,
                    HEADER_X_GEO_COORDINATES,
                    HEADER_X_GEO_METRO_CODE,
                    HEADER_X_GEO_INFO_AVAILABLE,
                ] {
                    if let Some(value) = req.get_header(header_name) {
                        response.set_header(header_name, value);
                    }
                }

                Ok(response)
            } else {
                log::warn!("Backend returned non-success status");
                Ok(Response::from_status(StatusCode::NO_CONTENT)
                    .with_header(header::CONTENT_TYPE, "application/json")
                    .with_header(HEADER_X_COMPRESS_HINT, "on")
                    .with_body("{}"))
            }
        }
        Err(e) => {
            log::error!("Error making backend request: {:?}", e);
            Ok(Response::from_status(StatusCode::NO_CONTENT)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_header(HEADER_X_COMPRESS_HINT, "on")
                .with_body("{}"))
        }
    }
}

/// Handles the prebid test route with detailed error logging
async fn handle_prebid_test(settings: &Settings, mut req: Request) -> Result<Response, Error> {
    log::info!("Starting prebid test request handling");

    // Check consent status from headers
    let advertising_consent = req
        .get_header(HEADER_X_CONSENT_ADVERTISING)
        .and_then(|h| h.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false);

    // Calculate fresh ID and synthetic ID only if we have advertising consent
    let (fresh_id, synthetic_id) = if advertising_consent {
        match (
            generate_synthetic_id(settings, &req),
            get_or_generate_synthetic_id(settings, &req),
        ) {
            (Ok(fresh), Ok(synth)) => (fresh, synth),
            (Err(e), _) | (_, Err(e)) => {
                log::error!("Failed to generate IDs: {:?}", e);
                return Ok(Response::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_header(header::CONTENT_TYPE, "application/json")
                    .with_body_json(&json!({
                        "error": "Failed to generate IDs",
                        "details": format!("{:?}", e)
                    }))?);
            }
        }
    } else {
        // Use non-personalized IDs when no consent
        (
            "non-personalized".to_string(),
            "non-personalized".to_string(),
        )
    };

    log::info!(
        "Existing Trusted Server header: {:?}",
        req.get_header(HEADER_SYNTHETIC_TRUSTED_SERVER)
    );
    log::info!("Generated Fresh ID: {}", &fresh_id);
    log::info!("Using Trusted Server ID: {}", synthetic_id);
    log::info!("Advertising consent: {}", advertising_consent);

    // Set both IDs as headers
    req.set_header(HEADER_SYNTHETIC_FRESH, &fresh_id);
    req.set_header(HEADER_SYNTHETIC_TRUSTED_SERVER, &synthetic_id);
    req.set_header(
        HEADER_X_CONSENT_ADVERTISING,
        if advertising_consent { "true" } else { "false" },
    );

    log::info!(
        "Using Trusted Server ID: {}, Fresh ID: {}",
        synthetic_id,
        fresh_id
    );

    let prebid_req = match PrebidRequest::new(settings, &req) {
        Ok(req) => {
            log::info!(
                "Successfully created PrebidRequest with synthetic ID: {}",
                req.synthetic_id
            );
            req
        }
        Err(e) => {
            log::error!("Error creating PrebidRequest: {:?}", e);
            return Ok(Response::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_body_json(&json!({
                    "error": "Failed to create prebid request",
                    "details": format!("{:?}", e)
                }))?);
        }
    };

    log::info!("Attempting to send bid request to Prebid Server at prebid_backend");

    match prebid_req.send_bid_request(settings, &req).await {
        Ok(mut prebid_response) => {
            log::info!("Received response from Prebid Server");
            log::info!("Response status: {}", prebid_response.get_status());

            log::info!("Response headers:");
            for (name, value) in prebid_response.get_headers() {
                log::info!("  {}: {:?}", name, value);
            }

            let body = prebid_response.take_body_str();
            log::info!("Response body: {}", body);

            Ok(Response::from_status(StatusCode::OK)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_header("X-Prebid-Test", "true")
                .with_header("X-Synthetic-ID", &prebid_req.synthetic_id)
                .with_header(
                    "X-Consent-Advertising",
                    if advertising_consent { "true" } else { "false" },
                )
                .with_header(HEADER_X_COMPRESS_HINT, "on")
                .with_body(body))
        }
        Err(e) => {
            log::error!("Error sending bid request: {:?}", e);
            log::error!("Backend name used: prebid_backend");
            Ok(Response::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_body_json(&json!({
                    "error": "Failed to send bid request",
                    "details": format!("{:?}", e),
                    "backend": "prebid_backend"
                }))?)
        }
    }
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
