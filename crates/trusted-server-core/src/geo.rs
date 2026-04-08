//! Geographic location utilities for the trusted server.
//!
//! This module provides a Fastly-to-core geo mapping helper and response-header
//! injection for the platform-neutral [`GeoInfo`] type.
//!
//! The [`GeoInfo`] data type is defined in [`crate::platform`] as platform-
//! neutral data; this module re-exports it and adds helper methods for HTTP
//! response header injection.

use edgezero_core::body::Body as EdgeBody;
use fastly::geo::Geo;
use http::{HeaderValue, Response};

pub use crate::platform::GeoInfo;

use crate::constants::{
    HEADER_X_GEO_CITY, HEADER_X_GEO_CONTINENT, HEADER_X_GEO_COORDINATES, HEADER_X_GEO_COUNTRY,
    HEADER_X_GEO_INFO_AVAILABLE, HEADER_X_GEO_METRO_CODE, HEADER_X_GEO_REGION,
};

/// Convert a Fastly [`Geo`] value into a platform-neutral [`GeoInfo`].
///
/// Shared by `FastlyPlatformGeo::lookup` in `trusted-server-adapter-fastly` so
/// that field mapping is never duplicated.
pub fn geo_from_fastly(geo: &Geo) -> GeoInfo {
    GeoInfo {
        city: geo.city().to_string(),
        country: geo.country_code().to_string(),
        continent: format!("{:?}", geo.continent()),
        latitude: geo.latitude(),
        longitude: geo.longitude(),
        metro_code: geo.metro_code(),
        region: geo.region().map(str::to_string),
    }
}

impl GeoInfo {
    /// Sets geo information headers on the response.
    ///
    /// Adds `x-geo-city`, `x-geo-country`, `x-geo-continent`, `x-geo-coordinates`,
    /// `x-geo-metro-code`, `x-geo-region` (when available), and
    /// `x-geo-info-available: true` to the given response.
    pub fn set_response_headers(&self, response: &mut Response<EdgeBody>) {
        let headers = response.headers_mut();

        insert_geo_header(headers, HEADER_X_GEO_CITY, &self.city);
        insert_geo_header(headers, HEADER_X_GEO_COUNTRY, &self.country);
        insert_geo_header(headers, HEADER_X_GEO_CONTINENT, &self.continent);
        insert_geo_header(
            headers,
            HEADER_X_GEO_COORDINATES,
            &self.coordinates_string(),
        );
        if self.has_metro_code() {
            let metro_code = self.metro_code.to_string();
            insert_geo_header(headers, HEADER_X_GEO_METRO_CODE, &metro_code);
        }
        if let Some(ref region) = self.region {
            insert_geo_header(headers, HEADER_X_GEO_REGION, region);
        }
        headers.insert(
            HEADER_X_GEO_INFO_AVAILABLE,
            HeaderValue::from_static("true"),
        );
    }
}

fn insert_geo_header(headers: &mut http::HeaderMap, name: http::header::HeaderName, value: &str) {
    match HeaderValue::from_str(value) {
        Ok(header_value) => {
            headers.insert(name, header_value);
        }
        Err(_) => {
            log::warn!("Skipping invalid geo header value for {}", name.as_str());
        }
    }
}

use std::collections::HashSet;
use std::sync::LazyLock;

/// EU-27 + EEA-3 (Iceland, Liechtenstein, Norway) + UK (UK GDPR).
///
/// Two-letter ISO 3166-1 alpha-2 country codes for jurisdictions where GDPR
/// or equivalent legislation applies. Used to infer GDPR applicability from
/// IP-derived geolocation when a more authoritative signal (e.g. TCF consent
/// string) is not yet available.
static GDPR_COUNTRIES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // EU-27
        "AT", "BE", "BG", "HR", "CY", "CZ", "DK", "EE", "FI", "FR", "DE", "GR", "HU", "IE", "IT",
        "LV", "LT", "LU", "MT", "NL", "PL", "PT", "RO", "SK", "SI", "ES", "SE",
        // EEA (non-EU)
        "IS", "LI", "NO", // UK GDPR
        "GB",
    ]
    .into_iter()
    .collect()
});

/// Returns `true` if the given two-letter country code falls under GDPR
/// jurisdiction (EU-27, EEA, or UK).
///
/// The comparison is case-insensitive. Returns `false` for empty or
/// unrecognised codes.
#[must_use]
pub fn is_gdpr_country(country_code: &str) -> bool {
    let upper = country_code.to_ascii_uppercase();
    GDPR_COUNTRIES.contains(upper.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use edgezero_core::body::Body as EdgeBody;
    use http::Response as HttpResponse;

    fn sample_geo_info() -> GeoInfo {
        GeoInfo {
            city: "San Francisco".to_string(),
            country: "US".to_string(),
            continent: "NorthAmerica".to_string(),
            latitude: 37.7749,
            longitude: -122.4194,
            metro_code: 807,
            region: Some("CA".to_string()),
        }
    }

    fn build_response() -> HttpResponse<EdgeBody> {
        HttpResponse::builder()
            .status(http::StatusCode::OK)
            .body(EdgeBody::empty())
            .expect("should build response")
    }

    #[test]
    fn set_response_headers_sets_all_geo_headers() {
        let geo = sample_geo_info();
        let mut response = build_response();

        geo.set_response_headers(&mut response);

        assert_eq!(
            response
                .headers()
                .get(HEADER_X_GEO_CITY)
                .expect("should have city header")
                .to_str()
                .expect("should be valid str"),
            "San Francisco",
            "should set city header"
        );
        assert_eq!(
            response
                .headers()
                .get(HEADER_X_GEO_COUNTRY)
                .expect("should have country header")
                .to_str()
                .expect("should be valid str"),
            "US",
            "should set country header"
        );
        assert_eq!(
            response
                .headers()
                .get(HEADER_X_GEO_CONTINENT)
                .expect("should have continent header")
                .to_str()
                .expect("should be valid str"),
            "NorthAmerica",
            "should set continent header"
        );
        assert_eq!(
            response
                .headers()
                .get(HEADER_X_GEO_COORDINATES)
                .expect("should have coordinates header")
                .to_str()
                .expect("should be valid str"),
            "37.7749,-122.4194",
            "should set coordinates header"
        );
        assert_eq!(
            response
                .headers()
                .get(HEADER_X_GEO_METRO_CODE)
                .expect("should have metro code header")
                .to_str()
                .expect("should be valid str"),
            "807",
            "should set metro code header"
        );
        assert_eq!(
            response
                .headers()
                .get(HEADER_X_GEO_REGION)
                .expect("should have region header")
                .to_str()
                .expect("should be valid str"),
            "CA",
            "should set region header"
        );
        assert_eq!(
            response
                .headers()
                .get(HEADER_X_GEO_INFO_AVAILABLE)
                .expect("should have info available header")
                .to_str()
                .expect("should be valid str"),
            "true",
            "should set geo info available to true"
        );
    }

    #[test]
    fn set_response_headers_omits_metro_code_when_zero() {
        let geo = GeoInfo {
            metro_code: 0,
            ..sample_geo_info()
        };
        let mut response = build_response();

        geo.set_response_headers(&mut response);

        assert!(
            response.headers().get(HEADER_X_GEO_METRO_CODE).is_none(),
            "should not set metro code header when metro_code is 0"
        );
        assert!(
            response.headers().get(HEADER_X_GEO_CITY).is_some(),
            "should still set city header"
        );
    }

    #[test]
    fn is_gdpr_country_detects_eu_members() {
        assert!(is_gdpr_country("DE"), "Germany is EU");
        assert!(is_gdpr_country("FR"), "France is EU");
        assert!(is_gdpr_country("IT"), "Italy is EU");
    }

    #[test]
    fn is_gdpr_country_detects_eea_and_uk() {
        assert!(is_gdpr_country("NO"), "Norway is EEA");
        assert!(is_gdpr_country("IS"), "Iceland is EEA");
        assert!(is_gdpr_country("GB"), "UK has UK GDPR");
    }

    #[test]
    fn is_gdpr_country_rejects_non_gdpr() {
        assert!(!is_gdpr_country("US"), "US is not GDPR");
        assert!(!is_gdpr_country("CN"), "China is not GDPR");
        assert!(!is_gdpr_country("BR"), "Brazil is not GDPR");
    }

    #[test]
    fn is_gdpr_country_is_case_insensitive() {
        assert!(is_gdpr_country("de"), "lowercase should match");
        assert!(is_gdpr_country("De"), "mixed case should match");
    }

    #[test]
    fn is_gdpr_country_handles_empty_and_unknown() {
        assert!(!is_gdpr_country(""), "empty string is not GDPR");
        assert!(!is_gdpr_country("XX"), "unknown code is not GDPR");
    }

    #[test]
    fn set_response_headers_omits_region_when_none() {
        let geo = GeoInfo {
            region: None,
            ..sample_geo_info()
        };
        let mut response = build_response();

        geo.set_response_headers(&mut response);

        assert!(
            response.headers().get(HEADER_X_GEO_REGION).is_none(),
            "should not set region header when region is None"
        );
        assert!(
            response.headers().get(HEADER_X_GEO_CITY).is_some(),
            "should still set city header"
        );
        assert_eq!(
            response
                .headers()
                .get(HEADER_X_GEO_INFO_AVAILABLE)
                .expect("should have info available header")
                .to_str()
                .expect("should be valid str"),
            "true",
            "should still set geo info available to true"
        );
    }
}
