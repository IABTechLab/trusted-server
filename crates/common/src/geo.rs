//! Geographic location utilities for the trusted server.
//!
//! This module provides functions for extracting and handling geographic
//! information from incoming requests, particularly DMA (Designated Market Area) codes.

use fastly::geo::geo_lookup;
use fastly::Request;

use crate::constants::{
    HEADER_X_GEO_CITY, HEADER_X_GEO_CONTINENT, HEADER_X_GEO_COORDINATES, HEADER_X_GEO_COUNTRY,
    HEADER_X_GEO_METRO_CODE,
};

/// Geographic information extracted from a request.
///
/// Contains all available geographic data from Fastly's geolocation service,
/// including city, country, continent, coordinates, and DMA/metro code.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GeoInfo {
    /// City name
    pub city: String,
    /// Two-letter country code (e.g., "US", "GB")
    pub country: String,
    /// Continent name
    pub continent: String,
    /// Latitude coordinate
    pub latitude: f64,
    /// Longitude coordinate
    pub longitude: f64,
    /// DMA (Designated Market Area) / metro code
    pub metro_code: i64,
    /// Region code
    pub region: Option<String>,
}

impl GeoInfo {
    /// Creates a new `GeoInfo` from a request by performing a geo lookup.
    ///
    /// This constructor performs a geo lookup based on the client's IP address and returns
    /// all available geographic data in a structured format. It does not modify the request
    /// or set headers.
    ///
    /// # Arguments
    ///
    /// * `req` - The request to extract geographic information from
    ///
    /// # Returns
    ///
    /// `Some(GeoInfo)` if geo data is available, `None` otherwise
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Some(geo_info) = GeoInfo::from_request(&req) {
    ///     println!("User is in {} ({})", geo_info.city, geo_info.country);
    ///     println!("Coordinates: {}", geo_info.coordinates_string());
    /// }
    /// ```
    pub fn from_request(req: &Request) -> Option<Self> {
        req.get_client_ip_addr()
            .and_then(geo_lookup)
            .map(|geo| GeoInfo {
                city: geo.city().to_string(),
                country: geo.country_code().to_string(),
                continent: format!("{:?}", geo.continent()),
                latitude: geo.latitude(),
                longitude: geo.longitude(),
                metro_code: geo.metro_code(),
                region: geo.region().map(str::to_string),
            })
    }

    /// Returns coordinates as a formatted string "latitude,longitude"
    #[must_use]
    pub fn coordinates_string(&self) -> String {
        format!("{},{}", self.latitude, self.longitude)
    }

    /// Checks if a valid metro code is available (non-zero)
    #[must_use]
    pub fn has_metro_code(&self) -> bool {
        self.metro_code > 0
    }

    /// Sets the geographic information headers on the given request.
    ///
    /// This sets the following headers:
    /// - `x-geo-city`
    /// - `x-geo-country`
    /// - `x-geo-continent`
    /// - `x-geo-coordinates`
    /// - `x-geo-metro-code`
    /// - `x-geo-region` (if available)
    pub fn set_headers(&self, req: &mut Request) {
        req.set_header(HEADER_X_GEO_CITY, &self.city);
        req.set_header(HEADER_X_GEO_COUNTRY, &self.country);
        req.set_header(HEADER_X_GEO_CONTINENT, &self.continent);
        req.set_header(HEADER_X_GEO_COORDINATES, self.coordinates_string());
        req.set_header(HEADER_X_GEO_METRO_CODE, self.metro_code.to_string());
        if let Some(region) = &self.region {
            req.set_header("x-geo-region", region);
        }
    }
}

/// Returns the geographic information for the request as a JSON response.
///
/// Use this endpoint to get the client's location data (City, Country, DMA, etc.)
/// without making a third-party API call.
///
/// # Errors
///
/// Returns a 500 error if JSON serialization fails (unlikely).
pub fn handle_first_party_geo(
    req: &Request,
) -> Result<fastly::Response, error_stack::Report<crate::error::TrustedServerError>> {
    use crate::error::TrustedServerError;
    use error_stack::ResultExt;
    use fastly::http::{header, StatusCode};
    use fastly::Response;

    let geo_info = GeoInfo::from_request(req);

    // Create a JSON response
    let body =
        serde_json::to_string(&geo_info).change_context(TrustedServerError::Serialization {
            message: "Failed to serialize geo info".to_string(),
        })?;

    Ok(Response::from_body(body)
        .with_status(StatusCode::OK)
        .with_header(header::CONTENT_TYPE, "application/json")
        .with_header("Cache-Control", "private, no-store"))
}
