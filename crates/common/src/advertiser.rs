//! Ad serving and advertiser integration functionality.
//!
//! This module handles ad requests, including GDPR consent checking,
//! synthetic ID generation, visitor tracking, and communication with
//! external ad partners.

use std::env;

use error_stack::Report;
use fastly::http::{header, StatusCode};
use fastly::{KVStore, Request, Response};

use crate::constants::{
    HEADER_X_COMPRESS_HINT, HEADER_X_CONSENT_ADVERTISING, HEADER_X_FORWARDED_FOR,
    HEADER_X_GEO_CITY, HEADER_X_GEO_CONTINENT, HEADER_X_GEO_COORDINATES, HEADER_X_GEO_COUNTRY,
    HEADER_X_GEO_INFO_AVAILABLE, HEADER_X_GEO_METRO_CODE,
};
use crate::error::TrustedServerError;
use crate::gdpr::{get_consent_from_request, GdprConsent};
use crate::geo::get_dma_code;
use crate::models::AdResponse;
use crate::settings::Settings;
use crate::synthetic::generate_synthetic_id;

/// Handles ad creative requests.
///
/// Processes ad requests with synthetic ID and consent checking.
///
/// # Errors
///
/// Returns a [`TrustedServerError`] if:
/// - Synthetic ID generation fails
/// - Backend communication fails
/// - Response creation fails
pub fn handle_ad_request(
    settings: &Settings,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
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
        generate_synthetic_id(settings, &req)?
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

    match ad_req.send(settings.ad_server.ad_partner_backend.as_str()) {
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
