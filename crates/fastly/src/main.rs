use std::env;

use http::header;
use http::status::StatusCode;
use http::Method;

use fastly::geo::geo_lookup;
use fastly::KVStore;
use fastly::{Error, Request as FastlyRequest, Response};
use serde_json::json;

pub mod http_wrapper;
use crate::http_wrapper::FastlyRequestWrapper;

use trusted_server_common::constants::{
    HEADER_SYNTHETIC_FRESH, HEADER_SYNTHETIC_TRUSTED_SERVER, HEADER_X_CONSENT_ADVERTISING,
    HEADER_X_FORWARDED_FOR, HEADER_X_GEO_CITY, HEADER_X_GEO_CONTINENT, HEADER_X_GEO_COORDINATES,
    HEADER_X_GEO_COUNTRY, HEADER_X_GEO_INFO_AVAILABLE, HEADER_X_GEO_METRO_CODE,
};
use trusted_server_common::cookies::create_synthetic_cookie;
use trusted_server_common::gdpr::{
    get_consent_from_request, handle_consent_request, handle_data_subject_request,
};
use trusted_server_common::geo::copy_geo_headers;
use trusted_server_common::http_wrapper::RequestWrapper;
use trusted_server_common::ip::get_client_ip;
use trusted_server_common::models::AdResponse;
use trusted_server_common::prebid::PrebidRequest;
use trusted_server_common::privacy::PRIVACY_TEMPLATE;
use trusted_server_common::request_id::{add_request_id_to_response, get_or_generate_request_id};
use trusted_server_common::settings::Settings;
use trusted_server_common::synthetic::{generate_synthetic_id, get_or_generate_synthetic_id};
use trusted_server_common::templates::HTML_TEMPLATE;
use trusted_server_common::why::WHY_TEMPLATE;

#[fastly::main]
fn main(mut fastly_req: FastlyRequest) -> Result<Response, Error> {
    // Initialize logging first
    trusted_server_common::logging::init_logging();

    let settings = match Settings::new() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Configuration error: {}", e);
            return Ok(Response::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                .with_body(format!("Configuration error: {}", e))
                .with_header(header::CONTENT_TYPE, "text/plain"));
        }
    };
    log::debug!("Settings loaded: {:?}", settings);

    futures::executor::block_on(async {
        let req = FastlyRequestWrapper::new(&mut fastly_req);
        let request_id = get_or_generate_request_id(&req);

        log::info!(
            "[{}] Request: {} {} - FASTLY_SERVICE_VERSION: {}",
            request_id,
            req.get_method(),
            req.get_path(),
            std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_else(|_| String::new())
        );

        let start = std::time::Instant::now();
        let mut response = match (req.get_method(), req.get_path()) {
            (&Method::GET, "/") => handle_main_page(&settings, req),
            (&Method::GET, "/health") => handle_health_check(),
            (&Method::GET, "/ad-creative") => handle_ad_request(&settings, req),
            (&Method::GET, "/prebid-test") => handle_prebid_test(&settings, req).await,
            (&Method::GET, "/gdpr/consent") => handle_consent_request(&settings, req),
            (&Method::POST, "/gdpr/consent") => handle_consent_request(&settings, req),
            (&Method::GET, "/gdpr/data") => handle_data_subject_request(&settings, req),
            (&Method::DELETE, "/gdpr/data") => handle_data_subject_request(&settings, req),
            (&Method::GET, "/privacy-policy") => Ok(Response::from_status(StatusCode::OK)
                .with_body(PRIVACY_TEMPLATE)
                .with_header(header::CONTENT_TYPE, "text/html")
                .with_header("x-compress-hint", "on")),
            (&Method::GET, "/why-trusted-server") => Ok(Response::from_status(StatusCode::OK)
                .with_body(WHY_TEMPLATE)
                .with_header(header::CONTENT_TYPE, "text/html")
                .with_header("x-compress-hint", "on")),
            _ => Ok(Response::from_status(StatusCode::NOT_FOUND)
                .with_body("Not Found")
                .with_header(header::CONTENT_TYPE, "text/plain")
                .with_header("x-compress-hint", "on")),
        }?;

        // Add request ID to response
        add_request_id_to_response(&mut response, &request_id);

        // Log request completion
        let elapsed = start.elapsed();
        log::info!(
            "[{}] Response: {} - Duration: {:?}",
            request_id,
            response.get_status(),
            elapsed
        );

        Ok(response)
    })
}

fn handle_health_check() -> Result<Response, Error> {
    let health_status = json!({
        "status": "healthy",
        "service": "trusted-server",
        "version": env!("CARGO_PKG_VERSION"),
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "fastly": {
            "pop": env::var("FASTLY_POP").unwrap_or_else(|_| "unknown".into()),
            "region": env::var("FASTLY_REGION").unwrap_or_else(|_| "unknown".into()),
            "service_version": env::var("FASTLY_SERVICE_VERSION").unwrap_or_else(|_| "unknown".into()),
        }
    });

    Ok(Response::from_status(StatusCode::OK)
        .with_header(header::CONTENT_TYPE, "application/json")
        .with_header(header::CACHE_CONTROL, "no-store, no-cache, must-revalidate")
        .with_body(serde_json::to_string(&health_status)?))
}

fn get_dma_code<T: RequestWrapper>(req: &mut T) -> Option<String> {
    // Debug: Check if we're running in Fastly environment
    log::debug!("Fastly Environment Check:");
    log::debug!(
        "  FASTLY_POP: {}",
        std::env::var("FASTLY_POP").unwrap_or_else(|_| "not in Fastly".into())
    );
    log::debug!(
        "  FASTLY_REGION: {}",
        std::env::var("FASTLY_REGION").unwrap_or_else(|_| "not in Fastly".into())
    );

    // Get detailed geo information using geo_lookup
    if let Some(geo) = req.get_client_ip_addr().and_then(geo_lookup) {
        log::debug!("Geo Information Found:");

        // Set all available geo information in headers
        let city = geo.city();
        req.set_header_str(HEADER_X_GEO_CITY, city);
        log::debug!("  City: {}", city);

        let country = geo.country_code();
        req.set_header_str(HEADER_X_GEO_COUNTRY, country);
        log::debug!("  Country: {}", country);

        req.set_header_str(HEADER_X_GEO_CONTINENT, &format!("{:?}", geo.continent()));
        log::debug!("  Continent: {:?}", geo.continent());

        req.set_header_str(
            HEADER_X_GEO_COORDINATES,
            &format!("{},{}", geo.latitude(), geo.longitude()),
        );
        log::debug!("  Location: ({}, {})", geo.latitude(), geo.longitude());

        // Get and set the metro code (DMA)
        let metro_code = geo.metro_code();
        req.set_header_str(HEADER_X_GEO_METRO_CODE, &metro_code.to_string());
        log::info!("Found DMA/Metro code: {}", metro_code);
        return Some(metro_code.to_string());
    } else {
        log::debug!("No geo information available for the request");
        req.set_header_str(HEADER_X_GEO_INFO_AVAILABLE, "false");
    }

    // If no metro code is found, log all request headers for debugging
    log::debug!("No DMA/Metro code found. All request headers:");
    for (name, value) in req.get_headers() {
        log::debug!("  {}: {:?}", name, value);
    }

    None
}

fn handle_main_page<T: RequestWrapper>(settings: &Settings, mut req: T) -> Result<Response, Error> {
    log::info!(
        "Using ad_partner_url: {}, counter_store: {}",
        settings.ad_server.ad_partner_url,
        settings.synthetic.counter_store,
    );

    // Add DMA code check to main page as well
    let dma_code = get_dma_code(&mut req);
    log::debug!("Main page - DMA Code: {:?}", dma_code);

    // Check GDPR consent before proceeding
    let consent = get_consent_from_request(&req).unwrap_or_default();
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
    let fresh_id = generate_synthetic_id(settings, &req);

    // Check for existing Trusted Server ID in this specific order:
    // 1. X-Synthetic-Trusted-Server header
    // 2. Cookie
    // 3. Fall back to fresh ID
    let synthetic_id = get_or_generate_synthetic_id(settings, &req);

    log::debug!(
        "Existing Truted Server header: {:?}",
        req.get_header(HEADER_SYNTHETIC_TRUSTED_SERVER)
    );
    log::debug!("Generated Fresh ID: {}", fresh_id);
    log::info!("Using Trusted Server ID: {}", synthetic_id);

    // Create response with the main page HTML
    let mut response = Response::from_status(StatusCode::OK)
        .with_body(HTML_TEMPLATE)
        .with_header(header::CONTENT_TYPE, "text/html")
        .with_header(HEADER_SYNTHETIC_FRESH, &fresh_id) // Fresh ID always changes
        .with_header(HEADER_SYNTHETIC_TRUSTED_SERVER, &synthetic_id) // Trusted Server ID remains stable
        .with_header(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            "X-Geo-City, X-Geo-Country, X-Geo-Continent, X-Geo-Coordinates, X-Geo-Metro-Code, X-Geo-Info-Available"
        )
        .with_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .with_header("x-compress-hint", "on");

    // Copy geo headers from request to response
    copy_geo_headers(&req, &mut response);

    // Only set cookies if we have consent
    if consent.functional {
        response.set_header(
            header::SET_COOKIE,
            create_synthetic_cookie(&synthetic_id, settings),
        );
    }

    // Debug: Print all request headers
    log::debug!("All Request Headers:");
    for (name, value) in req.get_headers() {
        log::debug!("{}: {:?}", name, value);
    }

    // Debug: Print the response headers
    log::debug!("Response Headers:");
    for (name, value) in response.get_headers() {
        log::debug!("{}: {:?}", name, value);
    }

    // Prevent caching
    response.set_header(header::CACHE_CONTROL, "no-store, private");

    Ok(response)
}

fn handle_ad_request<T: RequestWrapper>(
    settings: &Settings,
    mut req: T,
) -> Result<Response, Error> {
    // Check GDPR consent to determine if we should serve personalized or non-personalized ads
    let _consent = get_consent_from_request(&req).unwrap_or_default();
    let advertising_consent = req
        .get_header(HEADER_X_CONSENT_ADVERTISING)
        .and_then(|h| h.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false);

    // Add DMA code extraction
    let dma_code = get_dma_code(&mut req);

    log::info!("Client location - DMA Code: {:?}", dma_code);

    // Log headers for debugging
    let x_forwarded_for = req
        .get_header(HEADER_X_FORWARDED_FOR)
        .and_then(|h| h.to_str().ok());

    let client_ip = get_client_ip(x_forwarded_for);

    log::info!("Client IP: {}", client_ip);
    log::debug!("X-Forwarded-For: {}", x_forwarded_for.unwrap_or("None"));
    log::info!("Advertising consent: {}", advertising_consent);

    // Generate synthetic ID only if we have consent
    let synthetic_id = if advertising_consent {
        generate_synthetic_id(settings, &req)
    } else {
        // Use a generic ID for non-personalized ads
        "non-personalized".to_string()
    };

    // Only track visits if we have consent
    if advertising_consent {
        // Increment visit counter in KV store
        log::debug!("Opening KV store: {}", settings.synthetic.counter_store);
        if let Ok(Some(store)) = KVStore::open(settings.synthetic.counter_store.as_str()) {
            log::debug!("Fetching current count for synthetic ID: {}", synthetic_id);
            let current_count: i32 = store
                .lookup(&synthetic_id)
                .map(|mut val| match String::from_utf8(val.take_body_bytes()) {
                    Ok(s) => {
                        log::debug!("Value from KV store: {}", s);
                        Some(s)
                    }
                    Err(e) => {
                        log::error!("Error converting bytes to string: {}", e);
                        None
                    }
                })
                .map(|opt_s| {
                    log::debug!("Parsing string value: {:?}", opt_s);
                    opt_s.and_then(|s| s.parse().ok())
                })
                .unwrap_or_else(|_| {
                    log::debug!("No existing count found, starting at 0");
                    None
                })
                .unwrap_or(0);

            let new_count = current_count + 1;
            log::debug!("Incrementing count from {} to {}", current_count, new_count);

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
    let mut ad_req = FastlyRequest::get(ad_server_url);

    // Add consent information to the ad request
    ad_req.set_header(
        "X-Consent-Advertising",
        if advertising_consent { "true" } else { "false" },
    );

    log::debug!("Request headers to Equativ:");
    for (name, value) in ad_req.get_headers() {
        log::debug!("  {}: {:?}", name, value);
    }

    match ad_req.send(settings.ad_server.ad_partner_url.as_str()) {
        Ok(mut res) => {
            log::info!(
                "Received response from backend with status: {}",
                res.get_status()
            );

            // Extract Fastly PoP from the Compute environment
            let fastly_pop = env::var("FASTLY_POP").unwrap_or_else(|_| "unknown".into());
            let fastly_cache_generation =
                env::var("FASTLY_CACHE_GENERATION").unwrap_or_else(|_| "unknown".into());
            let fastly_customer_id =
                env::var("FASTLY_CUSTOMER_ID").unwrap_or_else(|_| "unknown".into());
            let fastly_hostname = env::var("FASTLY_HOSTNAME").unwrap_or_else(|_| "unknown".into());
            let fastly_region = env::var("FASTLY_REGION").unwrap_or_else(|_| "unknown".into());
            let fastly_service_id =
                env::var("FASTLY_SERVICE_ID").unwrap_or_else(|_| "unknown".into());
            // let fastly_service_version = env::var("FASTLY_SERVICE_VERSION").unwrap_or_else(|_| "unknown".into());
            let fastly_trace_id = env::var("FASTLY_TRACE_ID").unwrap_or_else(|_| "unknown".into());

            log::debug!("Fastly Jason PoP: {}", fastly_pop);
            log::debug!("Fastly Compute Variables:");
            log::debug!("  - FASTLY_CACHE_GENERATION: {}", fastly_cache_generation);
            log::debug!("  - FASTLY_CUSTOMER_ID: {}", fastly_customer_id);
            log::debug!("  - FASTLY_HOSTNAME: {}", fastly_hostname);
            log::debug!("  - FASTLY_POP: {}", fastly_pop);
            log::debug!("  - FASTLY_REGION: {}", fastly_region);
            log::debug!("  - FASTLY_SERVICE_ID: {}", fastly_service_id);
            //log::debug!("  - FASTLY_SERVICE_VERSION: {}", fastly_service_version);
            log::debug!("  - FASTLY_TRACE_ID: {}", fastly_trace_id);

            // Log all response headers
            log::debug!("Response headers from Equativ:");
            for (name, value) in res.get_headers() {
                log::debug!("  {}: {:?}", name, value);
            }

            if res.get_status().is_success() {
                let body = res.take_body_str();
                log::debug!("Backend response body: {}", body);

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
                            log::debug!("Found opid: {}", opid);

                            // Store in opid KV store
                            log::debug!(
                                "Attempting to open KV store: {}",
                                settings.synthetic.opid_store
                            );
                            match KVStore::open(settings.synthetic.opid_store.as_str()) {
                                Ok(Some(store)) => {
                                    log::debug!("Successfully opened KV store");
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
                    .with_header("x-compress-hint", "on")
                    .with_body(body);

                // Copy geo headers from request to response
                copy_geo_headers(&req, &mut response);

                // Attach PoP info to the response
                //response.set_header("X-Debug-Fastly-PoP", &fastly_pop);
                //log::debug!("Added X-Debug-Fastly-PoP: {}", fastly_pop);

                Ok(response)
            } else {
                log::warn!("Backend returned non-success status");
                Ok(Response::from_status(StatusCode::NO_CONTENT)
                    .with_header(header::CONTENT_TYPE, "application/json")
                    .with_header("x-compress-hint", "on")
                    .with_body("{}"))
            }
        }
        Err(e) => {
            log::error!("Error making backend request: {:?}", e);
            Ok(Response::from_status(StatusCode::NO_CONTENT)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_header("x-compress-hint", "on")
                .with_body("{}"))
        }
    }
}

/// Handles the prebid test route with detailed error logging
async fn handle_prebid_test<T: RequestWrapper>(
    settings: &Settings,
    mut req: T,
) -> Result<Response, Error> {
    log::info!("Starting prebid test request handling");

    // Check consent status from headers
    let advertising_consent = req
        .get_header(HEADER_X_CONSENT_ADVERTISING)
        .and_then(|h| h.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false);

    // Calculate fresh ID and synthetic ID only if we have advertising consent
    let (fresh_id, synthetic_id) = if advertising_consent {
        let fresh = generate_synthetic_id(settings, &req);
        let synth = get_or_generate_synthetic_id(settings, &req);
        (fresh, synth)
    } else {
        // Use non-personalized IDs when no consent
        (
            "non-personalized".to_string(),
            "non-personalized".to_string(),
        )
    };

    log::debug!(
        "Existing Trusted Server header: {:?}",
        req.get_header(HEADER_SYNTHETIC_TRUSTED_SERVER)
    );
    log::debug!("Generated Fresh ID: {}", fresh_id);
    log::info!("Using Trusted Server ID: {}", synthetic_id);
    log::info!("Advertising consent: {}", advertising_consent);

    // Set both IDs as headers
    req.set_header_str(HEADER_SYNTHETIC_FRESH, &fresh_id);
    req.set_header_str(HEADER_SYNTHETIC_TRUSTED_SERVER, &synthetic_id);
    req.set_header_str(
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
            log::debug!(
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

            log::debug!("Response headers:");
            for (name, value) in prebid_response.get_headers() {
                log::debug!("  {}: {:?}", name, value);
            }

            let body = prebid_response.take_body_str();
            log::debug!("Response body: {}", body);

            Ok(Response::from_status(StatusCode::OK)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_header("X-Prebid-Test", "true")
                .with_header("X-Synthetic-ID", &prebid_req.synthetic_id)
                .with_header(
                    "X-Consent-Advertising",
                    if advertising_consent { "true" } else { "false" },
                )
                .with_header("x-compress-hint", "on")
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
