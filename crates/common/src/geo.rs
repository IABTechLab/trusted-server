//! Geographic location utilities for the trusted server.
//!
//! This module provides functions for extracting and handling geographic
//! information from incoming requests, particularly DMA (Designated Market Area) codes.

use fastly::geo::geo_lookup;
use fastly::Request;

use crate::constants::{
    HEADER_X_GEO_CITY, HEADER_X_GEO_CONTINENT, HEADER_X_GEO_COORDINATES, HEADER_X_GEO_COUNTRY,
    HEADER_X_GEO_INFO_AVAILABLE, HEADER_X_GEO_METRO_CODE,
};

/// Extracts the DMA (Designated Market Area) code from the request's geolocation data.
///
/// This function:
/// 1. Checks if running in Fastly environment
/// 2. Performs geo lookup based on client IP
/// 3. Sets various geo headers on the request
/// 4. Returns the metro code (DMA) if available
///
/// # Arguments
///
/// * `req` - The request to extract DMA code from
///
/// # Returns
///
/// The DMA/metro code as a string if available, None otherwise
pub fn get_dma_code(req: &mut Request) -> Option<String> {
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
